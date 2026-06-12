# Sourceable helper: decide the bats `--jobs` flag based on which `parallel`
# is on PATH.
#
# bats-core can only parallelise with GNU parallel. The moreutils `parallel`
# is a different program; passing `--jobs` to bats while it is on PATH makes
# bats-core abort with "Cannot execute jobs without GNU parallel". So we detect
# the real GNU thing by its version banner and only then emit `--jobs 3`.
#
# Prints `--jobs 3` when GNU parallel is available, otherwise prints nothing.
detect_parallel_jobs_flag() {
  if parallel --version 2>/dev/null | grep -q 'GNU parallel'; then
    echo '--jobs 3'
  fi
}
