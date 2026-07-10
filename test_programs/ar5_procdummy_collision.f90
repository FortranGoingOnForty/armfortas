! Interface-block and PROCEDURE(...) dummy procedures named f stay scoped
! to their owning module procedure.
!
! CHECK: procdummy 16 23
module ar5_procdummy_collision_m
  implicit none
contains
  integer function apply2(f, a) result(r)
    interface
      integer function f(x) result(z)
        integer, intent(in) :: x
      end function f
    end interface
    integer, intent(in) :: a
    r = f(a) + f(a)
  end function apply2

  integer function chain(f) result(r)
    procedure(iface) :: f
    r = f(0)
  end function chain

  integer function iface(x) result(z)
    integer, intent(in) :: x
    z = x
  end function iface

  integer function plus5(x) result(z)
    integer, intent(in) :: x
    z = x + 5
  end function plus5

  integer function plus23(x) result(z)
    integer, intent(in) :: x
    z = x + 23
  end function plus23
end module ar5_procdummy_collision_m

program ar5_procdummy_collision
  use ar5_procdummy_collision_m
  implicit none
  print '(a,1x,i0,1x,i0)', 'procdummy', apply2(plus5, 3), chain(plus23)
end program ar5_procdummy_collision
