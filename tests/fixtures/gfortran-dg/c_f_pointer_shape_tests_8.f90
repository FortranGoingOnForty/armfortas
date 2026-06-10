! Imported from gcc testsuite gfortran.dg/c_f_pointer_shape_tests_8.f90
! (gcc commit b700707a77eeaa1d37f733c4b2d2e242063c29d2).
! Original directives: { dg-do compile } { dg-options "-std=f2023" } + 2x dg-error (LOWER must be INTEGER / rank 1)
! Conversion notes: tests/fixtures/gfortran-dg/README.md
! Wanted diagnostic: LOWER argument must be INTEGER and rank 1. Today
! armfortas accepts both calls with no diagnostic, so the XFAIL fires.
! Bare module (not runnable); ERROR_EXPECTED needs no runnable main.
! FLAGS: --std=f2023
! ERROR_EXPECTED: LOWER
! XFAIL: f2023 C_F_POINTER LOWER argument validation not implemented (l06); see .docs/audits/f2023-feature-matrix.md
! Verify that the type and rank of the LOWER argument are enforced.
module c_f_pointer_shape_tests_8
  use, intrinsic :: iso_c_binding

contains
  subroutine sub2(my_c_array) bind(c)
    type(c_ptr), value :: my_c_array
    integer(kind=c_int), dimension(:), pointer :: my_array_ptr

    call c_f_pointer(my_c_array, my_array_ptr, (/ 10 /), (/ 10.0 /)) ! original dg-error: "must be INTEGER"
  end subroutine sub2

  subroutine sub3(my_c_array) bind(c)
    type(c_ptr), value :: my_c_array
    integer(kind=c_int), dimension(:), pointer :: my_array_ptr
    integer(kind=c_int), dimension(1) :: shape
    integer(kind=c_int), dimension(1, 1) :: lower

    lower(1, 1) = 10
    call c_f_pointer(my_c_array, my_array_ptr, shape, lower) ! original dg-error: "must be of rank 1"
  end subroutine sub3
end module c_f_pointer_shape_tests_8
