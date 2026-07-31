! A SOURCE= expression belongs to the ALLOCATE statement, not to each
! allocation object.  Evaluate it exactly once before any allocation attempt,
! then reuse the value for every successful scalar or array destination.  The
! evaluation still occurs when an earlier allocation object reports failure.
!
! CHECK: ok
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|clean|repro
! IR_CHECK: call @afs_allocate_array
! IR_CHECK: call @afs_allocate_string
! IR_CHECK: rt_call @__afs_deallocate
program ar56_allocate_source_single_evaluation
  implicit none

  type :: payload
    integer :: value
  end type payload

  integer :: calls, character_array_calls
  common /ar56_character_array_calls/ character_array_calls

  interface
    function next_characters() result(value)
      character(:), allocatable :: value(:)
    end function next_characters
  end interface

  call check_scalar_source()
  call check_array_source()
  call check_complex_source()
  call check_derived_source()
  call check_character_source()
  call check_character_array_source()
  call check_failure_still_evaluates_source()
  print '(a)', 'ok'

contains

  integer function next_integer()
    calls = calls + 1
    next_integer = 40 + calls
  end function next_integer

  complex function next_complex()
    calls = calls + 1
    next_complex = cmplx(40 + calls, -(40 + calls))
  end function next_complex

  function next_payload() result(value)
    type(payload) :: value
    calls = calls + 1
    value%value = 40 + calls
  end function next_payload

  function next_character() result(value)
    character(:), allocatable :: value
    calls = calls + 1
    if (calls == 1) then
      value = 'v1 '
    else
      value = 'v2 '
    end if
  end function next_character

  subroutine check_scalar_source()
    integer, allocatable :: first, second

    calls = 0
    allocate(first, second, source=next_integer())
    if (calls /= 1) error stop 1
    if (first /= 41 .or. second /= 41) error stop 2
  end subroutine check_scalar_source

  subroutine check_array_source()
    integer, allocatable :: first(:), second(:)

    calls = 0
    allocate(first(2), second(3), source=next_integer())
    if (calls /= 1) error stop 3
    if (any(first /= 41) .or. any(second /= 41)) error stop 4
  end subroutine check_array_source

  subroutine check_complex_source()
    complex, allocatable :: first, second

    calls = 0
    allocate(first, second, source=next_complex())
    if (calls /= 1) error stop 5
    if (first /= cmplx(41, -41) .or. second /= cmplx(41, -41)) error stop 6
  end subroutine check_complex_source

  subroutine check_derived_source()
    type(payload), allocatable :: first, second

    calls = 0
    allocate(first, second, source=next_payload())
    if (calls /= 1) error stop 7
    if (first%value /= 41 .or. second%value /= 41) error stop 8
  end subroutine check_derived_source

  subroutine check_character_source()
    character(:), allocatable :: first, second

    calls = 0
    allocate(first, second, source=next_character())
    if (calls /= 1) error stop 9
    if (first /= 'v1 ' .or. second /= 'v1 ') error stop 10
  end subroutine check_character_source

  subroutine check_character_array_source()
    character(:), allocatable :: first(:), second(:)

    character_array_calls = 0
    allocate(first, second, source=next_characters())
    if (character_array_calls /= 1) error stop 11
    if (size(first) /= 2 .or. size(second) /= 2) error stop 12
    if (len(first) /= 3 .or. len(second) /= 3) error stop 13
    if (first(1) /= 'one' .or. first(2) /= 'two') error stop 14
    if (second(1) /= 'one' .or. second(2) /= 'two') error stop 15
  end subroutine check_character_array_source

  subroutine check_failure_still_evaluates_source()
    integer, allocatable :: first, second
    integer :: stat

    calls = 0
    allocate(first)
    first = -1
    allocate(first, second, source=next_integer(), stat=stat)
    if (calls /= 1) error stop 16
    if (stat == 0) error stop 17
    if (first /= -1 .or. allocated(second)) error stop 18
  end subroutine check_failure_still_evaluates_source

end program ar56_allocate_source_single_evaluation

function next_characters() result(value)
  implicit none

  integer :: character_array_calls
  character(:), allocatable :: value(:)
  common /ar56_character_array_calls/ character_array_calls

  character_array_calls = character_array_calls + 1
  allocate(character(3) :: value(2))
  value = ['one', 'two']
end function next_characters
