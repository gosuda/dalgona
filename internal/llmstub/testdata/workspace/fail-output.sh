#!/bin/sh
i=1
while [ "$i" -le 30 ]; do
    printf '%s\n' "$i"
    i=$((i + 1))
done
printf '%s\n' boom >&2
exit 3
