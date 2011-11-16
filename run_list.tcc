#include <iostream>		// FIXME: remove

#include <boost/optional.hpp>

//----------------------------------------------------------------

namespace {
	using namespace base;
	using namespace boost;
	using namespace std;

	template <typename T>
	bool overlaps_ordered(run<T> const &lhs, run<T> const &rhs) {
		return rhs.b_ < lhs.e_;
	}

	template <typename T>
	bool overlaps(run<T> const &lhs, run<T> const &rhs) {
		if (lhs.b_ <= rhs.b_)
			return overlaps_ordered(lhs, rhs);
		else
			return overlaps_ordered(rhs, lhs);
	}

	template <typename T>
	boost::optional<run<T> >
	merge_ordered_runs_if_overlapping(run<T> const &lhs, run<T> const &rhs) {
		typedef optional<run<T> > result;

		if (lhs.e_ < rhs.e_)
			return result(run<T>(lhs.b_, rhs.e_));

		if (lhs.e_ <= rhs.e_)
			return result(lhs);

		return result();
	}

	template <typename T>
	boost::optional<run<T> >
	merge_if_overlapping(run<T> const &lhs, run<T> const &rhs) {
		if (lhs.b_ <= rhs.b_)
			return merge_ordered_runs_if_overlapping(lhs, rhs);
		else
			return merge_ordered_runs_if_overlapping(rhs, lhs);
	}

	template <typename T>
	pair<typename set<run<T> >::const_iterator,
	     typename set<run<T> >::const_iterator>
	overlapping_range(set<run<T> > const &runs, run<T> const &r) {
		// FIXME: slow, but correct implementation first
		typedef typename set<run<T> >::const_iterator cit;

		for (cit b = runs.begin(); b != runs.end(); ++b) {
			if (overlaps(*b, r)) {
				cit e = b;
				++e;
				while (overlaps(*e, r))
					++e;

				return make_pair(b, e);
			}
		}

		return make_pair(runs.end(), runs.end());
	}
}

//----------------------------------------------------------------

template <typename T>
void
run_list<T>::add_run(T const &b, T const &e)
{
	using namespace std;
	typedef typename set<run<T> >::const_iterator cit;

	run<T> r(b, e);
	pair<cit, cit> range = overlapping_range(runs_, r);
	for (cit it = range.first; it != range.second; ++it) {
		optional<run<T> > mr = merge_if_overlapping(r, *it);
		if (mr)
			r = *mr;
	}

	runs_.erase(range.first, range.second);
	runs_.insert(r);
}

template <typename T>
void
run_list<T>::sub_run(T const &b, T const &e)
{
	// FIXME: finish
}

template <typename T>
bool
run_list<T>::in_run_(T const &key) const
{
	using namespace std;

	run<T> r(key, key + 1);
	typename set<run<T> >::const_iterator it = runs_.lower_bound(r);

	if (it != runs_.end() && it->b_ == key)
		return true;

	--it;
	if (it == runs_.end())
		return false;

	return it->b_ <= key && it->e_ > key;
}

template <typename T>
bool
run_list<T>::in_run(T const &key) const
{
	if (invert_)
		return !in_run_(key);
	else
		return in_run_(key);
}

template <typename T>
void
run_list<T>::invert()
{
	invert_ = !invert_;
}

template <typename T>
void
run_list<T>::add(run_list<T> const &rl)
{
	// FIXME: finish
}

template <typename T>
void
run_list<T>::sub(run_list<T> const &rl)
{
	// FIXME: finish
}

//----------------------------------------------------------------