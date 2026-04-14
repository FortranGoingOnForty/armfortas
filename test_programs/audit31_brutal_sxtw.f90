! XFAIL: codegen emits illegal sxtw Wd, Wn — Task #493
! CHECK: ok
! audit31 sxtw codegen repro
! Failure mode: compiler emits "sxtw w21, w20" (illegal — sxtw dest must be 64-bit).
! Extracted from fortsh io/suggestions.f90 character indexing pattern.
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
