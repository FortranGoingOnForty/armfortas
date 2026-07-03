! Regression (fpm get_command_line_settings): a character array-constructor
! assignment whose constructor both appends an array element AND adds a
! scalar character expression routes through the generic array-constructor
! descriptor path. The scalar element lowers to a Ptr<i8>, and storing it
! into the fixed-length [i8 x N] slot went through coerce_to_type's
! unhandled Ptr->Array fallback, which returned the pointer unchanged —
! corrupting the element (garbage, wrong length). The element must be
! copied with character assignment semantics (blank-padded).
program p
  implicit none
  character(len=8), allocatable :: a(:)
  character(len=8) :: extra
  integer :: i
  extra = 'new'
  a = [character(len=8) :: 'first', 'second']
  ! constructor contains the array `a` (flattened) plus a scalar expression
  a = [character(len=8) :: a, trim(extra)//'!']
  write(*, '(a,i0)') 'n=', size(a)
  do i = 1, size(a)
     write(*, '(a,a,a)') '|', trim(a(i)), '|'
  end do
  if (size(a) /= 3) error stop 1
  if (trim(a(1)) /= 'first') error stop 2
  if (trim(a(2)) /= 'second') error stop 3
  if (trim(a(3)) /= 'new!') error stop 4
end program p
! CHECK: n=3
! CHECK: |first|
! CHECK: |second|
! CHECK: |new!|
