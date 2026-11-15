#!/bin/sh
# Regenerate the golden files in this directory with ncdu 2.9.2, which must be on `PATH`.
#
# ncdu converts `source.json`, a hand-written export with fixed names, sizes, and times, so the
# golden files do not depend on the filesystem, the user, or the order of `readdir`. With the same
# ncdu build, only the timestamp in the metadata block of the JSON exports changes between runs.
set -eu

cd "$(dirname "$0")"

version=$(ncdu --version)
if [ "$version" != "ncdu 2.9.2" ]; then
    echo "ncdu 2.9.2 is required, found: $version" >&2
    exit 1
fi

convert() {
    input=$1
    shift
    ncdu --ignore-config -f "$input" "$@"
}

convert source.json -o tree.json
convert source.json -e -o tree-extended.json
convert source.json -e -c -o tree-extended.json.zst
convert source.json -O tree.ncdu
convert source.json -e -O tree-extended.ncdu

# A tree that spans several blocks of 4 KiB, with entries that refer to items in earlier blocks.
blocks=$(mktemp)
trap 'rm -f "$blocks"' EXIT
{
    printf '[1,2,{},\n[{"name":"/blocks","asize":4096,"dsize":4096}'
    for directory in 0 1 2 3 4 5 6; do
        printf ',\n[{"name":"directory-%s","asize":4096,"dsize":4096}' "$directory"
        index=$directory
        while [ "$index" -lt 300 ]; do
            printf ',\n{"name":"file-with-a-long-name-%s","asize":%s,"dsize":4096}' "$index" "$index"
            index=$((index + 7))
        done
        printf ']'
    done
    printf ']]\n'
} >"$blocks"
convert "$blocks" -c -o blocks.json.zst
convert "$blocks" --export-block-size 4 -O blocks.ncdu
