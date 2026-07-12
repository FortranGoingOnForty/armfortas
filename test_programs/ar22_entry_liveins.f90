! CHECK: ok
function ar22_pick_gp4(a, b, c, d) result(r) bind(c, name="ar22_pick_gp4")
  use iso_c_binding
  integer(c_int), value :: a, b, c, d
  integer(c_int) :: r
  r = d
end function ar22_pick_gp4

function ar22_pick_fp4(a, b, c, d) result(r) bind(c, name="ar22_pick_fp4")
  use iso_c_binding
  real(c_double), value :: a, b, c, d
  real(c_double) :: r
  r = d
end function ar22_pick_fp4

program ar22_entry_liveins
  use iso_c_binding
  implicit none

  interface
    function ar22_pick_gp4(a, b, c, d) result(r) bind(c, name="ar22_pick_gp4")
      import :: c_int
      integer(c_int), value :: a, b, c, d
      integer(c_int) :: r
    end function ar22_pick_gp4

    function ar22_pick_fp4(a, b, c, d) result(r) bind(c, name="ar22_pick_fp4")
      import :: c_double
      real(c_double), value :: a, b, c, d
      real(c_double) :: r
    end function ar22_pick_fp4
  end interface

  if (ar22_pick_gp4(11_c_int, 22_c_int, 33_c_int, 44_c_int) /= 44_c_int) then
    error stop 1
  end if
  if (ar22_pick_fp4(1.25_c_double, 2.5_c_double, 3.75_c_double, &
                     4.5_c_double) /= 4.5_c_double) then
    error stop 2
  end if
  print *, "ok"
end program ar22_entry_liveins
