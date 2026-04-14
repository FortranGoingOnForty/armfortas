! audit31 Finding 12: character indexing in some pattern made
! IntExtend emit an illegal `sxtw Wd, Wn` because isel always
! used SXTW regardless of source width. isel now picks by the
! SOURCE width (SXTB for 8, SXTH for 16, SXTW only for 32→64)
! and emits MOV for same-width "extends". Task #493.
! CHECK: DEFGH
module audit31_sxtw_mod
  implicit none
contains
  subroutine do_substring(buf, src, n, m)
    character(len=*), intent(inout) :: buf
    character(len=*), intent(in) :: src
    integer, intent(in) :: n, m
    integer :: j
    do j = 1, n
      buf(j:j) = src(m + j : m + j)
    end do
  end subroutine
end module

program test
  use audit31_sxtw_mod
  implicit none
  character(len=16) :: a, b
  a = 'xxxxxxxxxxxxxxxx'
  b = 'ABCDEFGHIJKLMNOP'
  call do_substring(a, b, 5, 3)
  print *, a
end program
