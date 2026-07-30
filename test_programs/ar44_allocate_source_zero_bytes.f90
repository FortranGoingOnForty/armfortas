! ALLOCATE(SOURCE=) accepts allocated and non-owning descriptors whose
! payload requires zero bytes. Zero bytes can arise from an empty shape,
! a zero-length character element, or a zero-size derived-type element.
!
! FLAGS: --std=f2023
! CHECK: 0 3 0 2 0
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
! IR_CHECK: call @afs_copy_array_data
program ar44_allocate_source_zero_bytes
  implicit none

  type :: empty_t
  end type empty_t

  integer :: fixed_empty(-4:-5)
  integer, allocatable :: allocated_empty(:)
  integer, allocatable :: empty_copy(:)
  integer, allocatable :: fixed_copy(:)
  character(len=0), allocatable :: zero_chars(:)
  character(len=0), allocatable :: char_copy(:)
  type(empty_t), allocatable :: zero_objects(:)
  type(empty_t), allocatable :: object_copy(:)

  allocate(allocated_empty(-4:-5))
  allocate(empty_copy, source=allocated_empty)
  if (.not. allocated(empty_copy)) error stop 1
  if (size(empty_copy) /= 0) error stop 2

  allocate(fixed_copy, source=fixed_empty)
  if (.not. allocated(fixed_copy)) error stop 3
  if (size(fixed_copy) /= 0) error stop 4

  allocate(zero_chars(-1:1))
  allocate(char_copy, source=zero_chars)
  if (.not. allocated(char_copy)) error stop 5
  if (size(char_copy) /= 3) error stop 6
  if (len(char_copy) /= 0) error stop 7

  allocate(zero_objects(2))
  allocate(object_copy, source=zero_objects)
  if (.not. allocated(object_copy)) error stop 8
  if (size(object_copy) /= 2) error stop 9

  print *, size(empty_copy), size(char_copy), len(char_copy), &
      size(object_copy), size(fixed_copy)
end program ar44_allocate_source_zero_bytes
