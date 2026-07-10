! CHECK: ok
! IR_CHECK: call @afs_char_array_assignment_requires_reallocation
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program alloc_char_conforming_assignment_bounds
  implicit none

  character(len=3), allocatable :: fixed_lhs(:), fixed_rhs(:)
  character(len=0), allocatable :: zero_lhs(:), zero_rhs(:)
  character(len=3), allocatable :: empty_lhs(:), empty_rhs(:)
  character(len=0), allocatable :: zero_empty_lhs(:), zero_empty_rhs(:)
  character(len=:), allocatable :: deferred_lhs(:), deferred_rhs(:)
  character(len=3), allocatable :: matrix_lhs(:, :), matrix_rhs(:, :)
  character(len=3), allocatable, target :: alias_lhs(:)
  character(len=3), pointer :: alias_view(:)

  allocate(fixed_lhs(0:1), fixed_rhs(5:6))
  fixed_rhs = ['abc', 'def']
  fixed_lhs = fixed_rhs
  if (lbound(fixed_lhs, 1) /= 0 .or. ubound(fixed_lhs, 1) /= 1) error stop 1
  if (fixed_lhs(0) /= 'abc' .or. fixed_lhs(1) /= 'def') error stop 2

  fixed_lhs = make_values()
  if (lbound(fixed_lhs, 1) /= 0 .or. ubound(fixed_lhs, 1) /= 1) error stop 12
  if (fixed_lhs(0) /= 'jkl' .or. fixed_lhs(1) /= 'mno') error stop 13

  deallocate(fixed_rhs)
  allocate(fixed_rhs(5:5))
  fixed_rhs = 'abc'
  fixed_lhs = fixed_rhs
  if (size(fixed_lhs) /= 1 .or. fixed_lhs(1) /= 'abc') error stop 3

  allocate(character(len=3) :: deferred_lhs(-2:-1), deferred_rhs(7:8))
  deferred_rhs = ['ghi', 'jkl']
  deferred_lhs = deferred_rhs
  if (lbound(deferred_lhs, 1) /= -2 .or. ubound(deferred_lhs, 1) /= -1) error stop 4
  if (deferred_lhs(-2) /= 'ghi' .or. deferred_lhs(-1) /= 'jkl') error stop 5

  deallocate(deferred_rhs)
  allocate(character(len=4) :: deferred_rhs(7:8))
  deferred_rhs = ['mnop', 'qrst']
  deferred_lhs = deferred_rhs
  if (len(deferred_lhs) /= 4) error stop 6
  if (deferred_lhs(lbound(deferred_lhs, 1)) /= 'mnop') error stop 6
  if (deferred_lhs(ubound(deferred_lhs, 1)) /= 'qrst') error stop 6

  allocate(matrix_lhs(0:1, -1:0), matrix_rhs(4:5, 7:8))
  matrix_rhs(4, 7) = 'one'
  matrix_rhs(5, 7) = 'two'
  matrix_rhs(4, 8) = 'six'
  matrix_rhs(5, 8) = 'ten'
  matrix_lhs = matrix_rhs
  if (lbound(matrix_lhs, 1) /= 0 .or. ubound(matrix_lhs, 1) /= 1) error stop 7
  if (lbound(matrix_lhs, 2) /= -1 .or. ubound(matrix_lhs, 2) /= 0) error stop 8
  if (matrix_lhs(0, -1) /= 'one' .or. matrix_lhs(1, -1) /= 'two') error stop 9
  if (matrix_lhs(0, 0) /= 'six' .or. matrix_lhs(1, 0) /= 'ten') error stop 10

  allocate(zero_lhs(-4:-3), zero_rhs(2:3))
  zero_lhs = zero_rhs
  if (lbound(zero_lhs, 1) /= -4 .or. ubound(zero_lhs, 1) /= -3) error stop 11

  allocate(empty_lhs(-4:-5), empty_rhs(2:1))
  empty_lhs = empty_rhs
  if (.not. allocated(empty_lhs) .or. size(empty_lhs) /= 0) error stop 14

  allocate(zero_empty_lhs(-4:-5), zero_empty_rhs(2:1))
  zero_empty_lhs = zero_empty_rhs
  if (.not. allocated(zero_empty_lhs) .or. size(zero_empty_lhs) /= 0) error stop 17

  allocate(alias_lhs(1:3))
  alias_lhs = ['abc', 'def', 'ghi']
  alias_view => alias_lhs
  alias_lhs = alias_view(3:1:-1)
  if (alias_lhs(1) /= 'ghi' .or. alias_lhs(2) /= 'def') error stop 15
  if (alias_lhs(3) /= 'abc') error stop 16

  print '(a)', 'ok'

contains
  function make_values() result(values)
    character(len=3), allocatable :: values(:)

    allocate(values(5:6))
    values = ['jkl', 'mno']
  end function make_values
end program alloc_char_conforming_assignment_bounds
