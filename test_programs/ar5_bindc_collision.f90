! CHECK: bindc OK
! MULTIFILE_LINK: ar5_bindc_collision_helper.f90 ar5_bindc_collision_main.f90

!--- file: ar5_bindc_collision_helper.f90
integer(c_int) function c_get_absolute_path(path, result, result_len) bind(c, name="get_absolute_path")
  use iso_c_binding
  implicit none
  type(c_ptr), value :: path
  character(kind=c_char), dimension(*) :: result
  integer(c_int), value :: result_len

  c_get_absolute_path = 1_c_int
  if (result_len >= 3_c_int) then
    result(1) = 'O'
    result(2) = 'K'
    result(3) = c_null_char
  end if
end function c_get_absolute_path

!--- file: ar5_bindc_collision_main.f90
module ar5_bindc_collision_fs
  use iso_c_binding
  implicit none

  interface
    function get_abs_helper(path, result, result_len) bind(c, name="get_absolute_path")
      use iso_c_binding
      type(c_ptr), value :: path
      character(kind=c_char), dimension(*) :: result
      integer(c_int), value :: result_len
      integer(c_int) :: get_abs_helper
    end function get_abs_helper
  end interface

contains
  function get_absolute_path(path) result(abs_path)
    character(len=*), intent(in) :: path
    character(len=:), allocatable :: abs_path
    character(kind=c_char, len=16), target :: c_path, c_result
    integer(c_int) :: rc

    c_path = trim(path) // c_null_char
    rc = get_abs_helper(c_loc(c_path), c_result, 16_c_int)
    if (rc == 0_c_int) then
      abs_path = 'FAIL'
    else
      abs_path = c_result(1:2)
    end if
  end function get_absolute_path
end module ar5_bindc_collision_fs

program ar5_bindc_collision
  use ar5_bindc_collision_fs
  implicit none
  print '(a,1x,a)', 'bindc', get_absolute_path('.')
end program ar5_bindc_collision
