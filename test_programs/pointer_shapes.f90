! Pointer variables at every storable shape: scalar, array
! (deferred shape, descriptor-backed), and derived type.  Covers
! =>, dereference-on-read, dereference-on-write, component access
! through a pointer, runtime size on an array pointer, and
! ASSOCIATED status for every shape.
program pointer_shapes
  implicit none
  type :: pt
    integer :: x, y
  end type
  integer, target :: sx = 10, sy = 20
  integer, pointer :: sp, sq
  integer, target :: ia(5) = [11, 22, 33, 44, 55]
  integer, pointer :: pa(:)
  type(pt), target :: ts
  type(pt), pointer :: tp

  ! Scalar pointer, plain deref, reassociation.
  print *, associated(sp)
  sp => sx
  print *, associated(sp), sp
  sp = 100
  print *, sx, sp
  sp => sy
  print *, sp

  ! Pointer-to-pointer copy.
  sq => sp
  print *, sq

  ! Array pointer: subscript, size, whole-array.
  print *, associated(pa)
  pa => ia
  print *, associated(pa), size(pa)
  print *, pa(1), pa(3), pa(5)

  ! Derived-type pointer: component access through the pointer.
  print *, associated(tp)
  ts%x = 7
  ts%y = 9
  tp => ts
  print *, associated(tp), tp%x, tp%y
  tp%x = 70
  print *, ts%x, tp%x
end program pointer_shapes
! CHECK: F
! CHECK: T 10
! CHECK: 100 100
! CHECK: 20
! CHECK: 20
! CHECK: F
! CHECK: T 5
! CHECK: 11 33 55
! CHECK: F
! CHECK: T 7 9
! CHECK: 70 70
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
