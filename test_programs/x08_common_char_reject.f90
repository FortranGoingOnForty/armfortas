! x08: character members of COMMON blocks are rejected loudly — the
! lowering gave them a pointer slot instead of inline bytes, so every
! read came back empty (silent wrong answer on all targets). Inline
! character storage lands with l06's string-representation work.
! ERROR_EXPECTED: character member 'tag' in a COMMON block is not supported
program x08_common_char_reject
  implicit none
  character(4) :: tag
  integer :: count
  common /shared/ tag, count
  tag = "abcd"
  count = 1
  print *, tag, count
end program x08_common_char_reject
