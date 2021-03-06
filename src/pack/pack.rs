use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};

use flate2::{read::ZlibDecoder, write::ZlibEncoder, Compression};

use std::os::unix::fs::OpenOptionsExt;
use std::{
    error::Error,
    fs::OpenOptions,
    io,
    io::prelude::*,
    io::Cursor,
    io::Write,
    ops::DerefMut,
    sync::{Arc, Mutex},
    thread::spawn,
};

use rand::prelude::*;
use std::sync::mpsc::{sync_channel, Receiver};

use crate::pack::node_encode::*;

const BLOCK_SIZE: u64 = 4096;
const MAGIC: u64 = 0xa537a0aa6309ef77;
const PACK_VERSION: u64 = 3;
const SUPERBLOCK_CSUM_XOR: u32 = 160774;
const BITMAP_CSUM_XOR: u32 = 240779;
const INDEX_CSUM_XOR: u32 = 160478;
const BTREE_CSUM_XOR: u32 = 121107;

fn shuffle<T>(v: &mut Vec<T>) {
    let mut rng = rand::thread_rng();
    v.shuffle(&mut rng);
}

// FIXME: move to a utils module
fn div_up(n: u64, d: u64) -> u64 {
    (n + d - 1) / d
}

// Each thread processes multiple contiguous runs of blocks, called
// chunks.  Chunks are shuffled so each thread gets chunks spread
// across the dev in case there are large regions that don't contain
// metadata.
fn mk_chunk_vecs(nr_blocks: u64, nr_jobs: u64) -> Vec<Vec<(u64, u64)>> {
    use std::cmp::{max, min};

    let chunk_size = min(4 * 1024u64, max(128u64, nr_blocks / (nr_jobs * 64)));
    let nr_chunks = div_up(nr_blocks, chunk_size);
    let mut chunks = Vec::with_capacity(nr_chunks as usize);
    for i in 0..nr_chunks {
        chunks.push((i * chunk_size, (i + 1) * chunk_size));
    }

    shuffle(&mut chunks);

    let mut vs = Vec::with_capacity(nr_jobs as usize);
    for _ in 0..nr_jobs {
        vs.push(Vec::new());
    }

    for c in 0..nr_chunks {
        vs[(c % nr_jobs) as usize].push(chunks[c as usize]);
    }

    vs
}

pub fn pack(input_file: &str, output_file: &str) -> Result<(), Box<dyn Error>> {
    let nr_blocks = get_nr_blocks(&input_file)?;
    let nr_jobs = std::cmp::max(1, std::cmp::min(num_cpus::get() as u64, nr_blocks / 128));
    let chunk_vecs = mk_chunk_vecs(nr_blocks, nr_jobs);

    let input = OpenOptions::new()
        .read(true)
        .write(false)
        .custom_flags(libc::O_EXCL)
        .open(input_file)?;

    let output = OpenOptions::new()
        .read(false)
        .write(true)
        .create(true)
        .truncate(true)
        .open(output_file)?;

    write_header(&output, nr_blocks)?;

    let sync_input = Arc::new(Mutex::new(input));
    let sync_output = Arc::new(Mutex::new(output));

    let mut threads = Vec::new();
    for job in 0..nr_jobs {
        let sync_input = Arc::clone(&sync_input);
        let sync_output = Arc::clone(&sync_output);
        let chunks = chunk_vecs[job as usize].clone();
        threads.push(spawn(move || crunch(sync_input, sync_output, chunks)));
    }

    for t in threads {
        t.join().unwrap()?;
    }
    Ok(())
}

fn crunch<R, W>(
    input: Arc<Mutex<R>>,
    output: Arc<Mutex<W>>,
    ranges: Vec<(u64, u64)>,
) -> io::Result<()>
where
    R: Read + Seek,
    W: Write,
{
    let mut written = 0u64;
    let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
    for (lo, hi) in ranges {
        // We read multiple blocks at once to reduce contention
        // on input.
        let mut input = input.lock().unwrap();
        let big_data = read_blocks(input.deref_mut(), lo, hi - lo)?;
        drop(input);

        for b in lo..hi {
            let block_start = ((b - lo) * BLOCK_SIZE) as usize;
            let data = &big_data[block_start..(block_start + BLOCK_SIZE as usize)];
            let kind = metadata_block_type(data);
            if kind != BT::UNKNOWN {
                z.write_u64::<LittleEndian>(b)?;
                pack_block(&mut z, kind, &data);

                written += 1;
                if written == 1024 {
                    let compressed = z.reset(Vec::new())?;

                    let mut output = output.lock().unwrap();
                    output.write_u64::<LittleEndian>(compressed.len() as u64)?;
                    output.write_all(&compressed)?;
                    written = 0;
                }
            }
        }
    }

    if written > 0 {
        let compressed = z.finish()?;
        let mut output = output.lock().unwrap();
        output.write_u64::<LittleEndian>(compressed.len() as u64)?;
        output.write_all(&compressed)?;
    }

    Ok(())
}

fn write_header<W>(mut w: W, nr_blocks: u64) -> io::Result<()>
where
    W: byteorder::WriteBytesExt,
{
    w.write_u64::<LittleEndian>(MAGIC)?;
    w.write_u64::<LittleEndian>(PACK_VERSION)?;
    w.write_u64::<LittleEndian>(4096)?;
    w.write_u64::<LittleEndian>(nr_blocks)?;

    Ok(())
}

fn read_header<R>(mut r: R) -> io::Result<u64>
where
    R: byteorder::ReadBytesExt,
{
    let magic = r.read_u64::<LittleEndian>()?;
    assert_eq!(magic, MAGIC);
    let version = r.read_u64::<LittleEndian>()?;
    assert_eq!(version, PACK_VERSION);
    let block_size = r.read_u64::<LittleEndian>()?;
    assert_eq!(block_size, 4096);
    r.read_u64::<LittleEndian>()
}

fn get_nr_blocks(path: &str) -> io::Result<u64> {
    let metadata = std::fs::metadata(path)?;
    Ok(metadata.len() / (BLOCK_SIZE as u64))
}

fn read_blocks<R>(rdr: &mut R, b: u64, count: u64) -> io::Result<Vec<u8>>
where
    R: io::Read + io::Seek,
{
    let mut buf: Vec<u8> = vec![0; (BLOCK_SIZE * count) as usize];

    rdr.seek(io::SeekFrom::Start(b * BLOCK_SIZE))?;
    rdr.read_exact(&mut buf)?;

    Ok(buf)
}

fn checksum(buf: &[u8]) -> u32 {
    crc32c::crc32c(&buf[4..]) ^ 0xffffffff
}

#[derive(PartialEq)]
enum BT {
    SUPERBLOCK,
    BTREE,
    INDEX,
    BITMAP,
    UNKNOWN,
}

fn metadata_block_type(buf: &[u8]) -> BT {
    if buf.len() != BLOCK_SIZE as usize {
        return BT::UNKNOWN;
    }

    // The checksum is always stored in the first u32 of the buffer.
    let mut rdr = Cursor::new(buf);
    let sum_on_disk = rdr.read_u32::<LittleEndian>().unwrap();
    let csum = checksum(buf);
    let btype = csum ^ sum_on_disk;

    match btype {
        SUPERBLOCK_CSUM_XOR => return BT::SUPERBLOCK,
        BTREE_CSUM_XOR => return BT::BTREE,
        BITMAP_CSUM_XOR => return BT::BITMAP,
        INDEX_CSUM_XOR => return BT::INDEX,
        _ => {
            return BT::UNKNOWN;
        }
    }
}

fn check<T>(r: &PResult<T>) {
    match r {
        Ok(_) => {
            return;
        }
        Err(PackError::ParseError) => panic!("parse error"),
        Err(PackError::IOError) => panic!("io error"),
    }
}

fn pack_block<W: Write>(w: &mut W, kind: BT, buf: &[u8]) {
    match kind {
        BT::SUPERBLOCK => check(&pack_superblock(w, buf)),
        BT::BTREE => check(&pack_btree_node(w, buf)),
        BT::INDEX => check(&pack_index(w, buf)),
        BT::BITMAP => check(&pack_bitmap(w, buf)),
        BT::UNKNOWN => {
            assert!(false);
        }
    }
}

fn write_zero_block<W>(w: &mut W, b: u64) -> io::Result<()>
where
    W: Write + Seek,
{
    let zeroes: Vec<u8> = vec![0; BLOCK_SIZE as usize];
    w.seek(io::SeekFrom::Start(b * BLOCK_SIZE))?;
    w.write_all(&zeroes)?;
    Ok(())
}

fn write_blocks<W>(w: &Arc<Mutex<W>>, blocks: &mut Vec<(u64, Vec<u8>)>) -> io::Result<()>
where
    W: Write + Seek,
{
    let mut w = w.lock().unwrap();
    while let Some((b, block)) = blocks.pop() {
        w.seek(io::SeekFrom::Start(b * BLOCK_SIZE))?;
        w.write_all(&block[0..])?;
    }
    Ok(())
}

fn decode_worker<W>(rx: Receiver<Vec<u8>>, w: Arc<Mutex<W>>) -> io::Result<()>
where
    W: Write + Seek,
{
    let mut blocks = Vec::new();

    while let Ok(bytes) = rx.recv() {
        let mut z = ZlibDecoder::new(&bytes[0..]);

        while let Ok(b) = z.read_u64::<LittleEndian>() {
            let block = crate::pack::vm::unpack(&mut z, BLOCK_SIZE as usize).unwrap();
            assert!(metadata_block_type(&block[0..]) != BT::UNKNOWN);
            blocks.push((b, block));

            if blocks.len() >= 32 {
                write_blocks(&w, &mut blocks)?;
            }
        }
    }

    write_blocks(&w, &mut blocks)?;
    Ok(())
}

pub fn unpack(input_file: &str, output_file: &str) -> Result<(), Box<dyn Error>> {
    let mut input = OpenOptions::new()
        .read(true)
        .write(false)
        .open(input_file)?;

    let mut output = OpenOptions::new()
        .read(false)
        .write(true)
        .create(true)
        .truncate(true)
        .open(output_file)?;

    let nr_blocks = read_header(&input)?;

    // zero the last block to size the file
    write_zero_block(&mut output, nr_blocks - 1)?;

    // Run until we hit the end
    let output = Arc::new(Mutex::new(output));

    // kick off the workers
    let nr_jobs = num_cpus::get();
    let mut senders = Vec::new();
    let mut threads = Vec::new();

    for _ in 0..nr_jobs {
        let (tx, rx) = sync_channel(1);
        let output = Arc::clone(&output);
        senders.push(tx);
        threads.push(spawn(move || decode_worker(rx, output)));
    }

    // Read z compressed chunk, and hand to worker thread.
    let mut next_worker = 0;
    while let Ok(len) = input.read_u64::<LittleEndian>() {
        let mut bytes = vec![0; len as usize];
        input.read_exact(&mut bytes)?;
        senders[next_worker].send(bytes).unwrap();
        next_worker = (next_worker + 1) % nr_jobs;
    }

    for s in senders {
        drop(s);
    }

    for t in threads {
        t.join().unwrap()?;
    }
    Ok(())
}
