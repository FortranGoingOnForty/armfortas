! Imported from gcc testsuite gfortran.dg/conditional_6.f90
! (gcc commit b700707a77eeaa1d37f733c4b2d2e242063c29d2).
! Original directives: { dg-do run } { dg-options "-std=f2023" }
! Conversion notes: tests/fixtures/gfortran-dg/README.md
! FLAGS: --std=f2023
! EXIT_CODE: 0
! XFAIL: f2023 conditional expressions not implemented (l02); see .docs/audits/f2023-feature-matrix.md
program conditional_arg
  implicit none
  integer :: a = 4
  integer :: b = 5
  character(kind=1, len=4) :: c4 = "abcd"
  character(kind=1, len=5) :: c5 = "bcdef"

  call five((a < 5 ? a : b))
  if (a /= 5) stop 1

  if (my_trim_len((b == 5 ? c4 : c5)) /= 4) stop 2
  if (my_trim_len((b == 5 ? "abcd" : "abcde")) /= 4) stop 3
  if (my_trim_len((b /= 5 ? c4 : c5)) /= 5) stop 4
  if (my_trim_len((b /= 5 ? "abcd" : "abcde")) /= 5) stop 5

  call five_c((b == 5 ? c4 : c5))
  if (c4 /= "bcde") stop 6
contains
  subroutine five(x)
    integer, optional :: x
    if (present(x)) then
      x = 5
    end if
  end subroutine five

  integer function my_trim_len(s)
    character(len=*), intent(in) :: s
    my_trim_len = len_trim(s)
  end function my_trim_len

  subroutine five_c(x)
    character(len=*), optional :: x
    if (present(x)) then
      x = c5
    end if
  end subroutine five_c
end program conditional_arg
