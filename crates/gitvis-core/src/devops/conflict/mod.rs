//! Merge conflicts: the three versions of a conflicted file (base, ours,
//! theirs from the index stages), conflict hunks parsed from the working
//! file's markers (including diff3 / zdiff3 style), per-hunk resolution
//! (ours, theirs, both, base, edited), writing the result and marking it
//! resolved.
