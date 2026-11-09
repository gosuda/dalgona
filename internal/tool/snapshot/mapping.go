package snapshot

func MapSpan(target Span, edit Splice) (Span, bool) {
	if edit.Start == edit.End {
		if target.Start < edit.Start && edit.Start < target.End {
			return Span{}, false
		}
		if edit.Start <= target.Start {
			delta := edit.Inserted
			return Span{Start: target.Start + delta, End: target.End + delta}, true
		}
		return target, true
	}
	if max(target.Start, edit.Start) < min(target.End, edit.End) {
		return Span{}, false
	}
	if edit.End <= target.Start {
		delta := edit.Inserted - (edit.End - edit.Start)
		return Span{Start: target.Start + delta, End: target.End + delta}, true
	}
	return target, true
}

func MapGap(gap int, edit Splice) (int, bool) {
	if edit.Start <= gap && gap <= edit.End {
		return 0, false
	}
	if edit.End < gap {
		return gap + edit.Inserted - (edit.End - edit.Start), true
	}
	return gap, true
}
