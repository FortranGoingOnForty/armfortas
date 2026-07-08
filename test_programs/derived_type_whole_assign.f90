! Whole-struct assignment `b = a` on a derived-type variable.
!
! Previously lower_expr's Name branch reached load_typed(info.addr,
! info.ty) for a derived-type local, loading the first 8 bytes of
! the struct as if they were a pointer (info.ty is the Ptr<i8>
! marker). The assignment path then memcpy'd `sizeof(type)` bytes
! from that garbage pointer into the destination, so only the
! byte-overlap with `a`'s first component survived. At O2 the
! garbage pointer often pointed outside the program image and the
! program segfaulted.
!
! Fix: treat a derived-type Name as an address-valued expression,
! symmetric to the existing complex/array cases — `info.addr` is
! already the address of the struct.
program derived_type_whole_assign
  implicit none
  type :: pt
    integer :: x, y
    real :: v
  end type
  type(pt) :: a, b

  a%x = 1
  a%y = 2
  a%v = 3.5

  b = a
  b%x = 100

  print *, a%x, a%y, a%v
  print *, b%x, b%y, b%v
end program derived_type_whole_assign
! CHECK: 1           2     3.5000000E0
! CHECK: 100           2     3.5000000E0
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
