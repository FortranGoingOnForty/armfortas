! l02 regression: a main program without a PROGRAM statement may carry
! internal procedures (F2018 R1401). The implicit-main parser used to
! leave CONTAINS unconsumed and parse_file spun on it, allocating
! program units until OOM (55GB resident observed; conditional_8.f90
! triggered it once l02's '?' lexing let parsing reach that line). A
! progress guard in parse_file now turns any such bug into a parse
! error, and this fixture pins the legal form end to end.
! CHECK: 12
implicit none
integer :: aa(2)
aa = [1, 2]
print *, h(aa(1)) + h(aa(2))
contains
integer function h(x)
  integer, intent(in) :: x
  h = 4 * x
end function h
end
