! Scalar pointer allocation uses a raw pointer slot rather than an array
! descriptor.  SOURCE= must still initialize the target exactly as intrinsic
! assignment would, including deep copies of allocatable components.  MOLD=
! and source-less allocation retain declaration-default initialization.
!
! CHECK: ok
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|clean|repro
! IR_CHECK: call @afs_allocate_scalar
! IR_CHECK: call @afs_assign_char_fixed
program ar56_pointer_allocate_source
  implicit none

  type :: payload
    integer :: value = -99
    integer, allocatable :: data(:)
  end type payload

  type :: holder
    integer, pointer :: scalar => null()
    character(4), pointer :: text => null()
    type(payload), pointer :: object => null()
  end type holder

  integer, pointer :: scalar, descriptor_scalar
  integer, pointer :: first, second
  integer, allocatable :: descriptor_integer
  complex, pointer :: z, descriptor_z
  complex, allocatable :: descriptor_complex
  character(4), pointer :: text
  type(payload), pointer :: object, descriptor_object, default_object, mold_object
  type(payload), allocatable :: descriptor_source
  type(payload) :: source
  type(holder) :: box
  integer :: calls

  source%value = 41
  allocate(source%data(2))
  source%data = [7, 8]

  allocate(descriptor_source)
  descriptor_source%value = 51
  allocate(descriptor_source%data(2))
  descriptor_source%data = [17, 18]
  allocate(descriptor_integer, source=44)
  allocate(descriptor_complex, source=cmplx(-2.0, 5.0))

  allocate(scalar, source=42)
  allocate(descriptor_scalar, source=descriptor_integer)
  allocate(z, source=cmplx(3.0, -4.0))
  allocate(descriptor_z, source=descriptor_complex)
  allocate(text, source='xy  ')
  allocate(object, source=source)
  allocate(descriptor_object, source=descriptor_source)
  allocate(box%scalar, source=43)
  allocate(box%text, source='box ')
  allocate(box%object, source=source)
  allocate(default_object)
  allocate(mold_object, mold=source)

  if (.not. associated(scalar) .or. scalar /= 42) error stop 1
  if (.not. associated(descriptor_scalar) .or. descriptor_scalar /= 44) error stop 2
  if (.not. associated(z)) error stop 3
  if (real(z) /= 3.0 .or. aimag(z) /= -4.0) error stop 4
  if (.not. associated(descriptor_z)) error stop 25
  if (real(descriptor_z) /= -2.0 .or. aimag(descriptor_z) /= 5.0) error stop 26
  if (.not. associated(text) .or. text /= 'xy  ') error stop 5
  if (.not. associated(object)) error stop 6
  if (object%value /= 41 .or. any(object%data /= [7, 8])) error stop 7
  if (.not. associated(descriptor_object)) error stop 8
  if (descriptor_object%value /= 51) error stop 9
  if (any(descriptor_object%data /= [17, 18])) error stop 10
  if (.not. associated(box%scalar) .or. box%scalar /= 43) error stop 11
  if (.not. associated(box%text) .or. box%text /= 'box ') error stop 12
  if (.not. associated(box%object)) error stop 13
  if (box%object%value /= 41 .or. any(box%object%data /= [7, 8])) error stop 14
  if (default_object%value /= -99 .or. allocated(default_object%data)) error stop 15
  if (mold_object%value /= -99 .or. allocated(mold_object%data)) error stop 16

  source%data = -1
  descriptor_source%data = -2
  descriptor_integer = -3
  descriptor_complex = cmplx(9.0, 9.0)
  if (object%value /= 41 .or. any(object%data /= [7, 8])) error stop 17
  if (any(descriptor_object%data /= [17, 18])) error stop 18
  if (descriptor_scalar /= 44) error stop 19
  if (any(box%object%data /= [7, 8])) error stop 20
  if (real(descriptor_z) /= -2.0 .or. aimag(descriptor_z) /= 5.0) error stop 27

  calls = 0
  allocate(first, second, source=next_integer())
  if (calls /= 1) error stop 21
  if (first /= 61 .or. second /= 61) error stop 22

  print '(a)', 'ok'

contains

  integer function next_integer()
    calls = calls + 1
    next_integer = 60 + calls
  end function next_integer

end program ar56_pointer_allocate_source
