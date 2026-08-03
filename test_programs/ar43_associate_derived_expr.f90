! A derived expression used as an ASSOCIATE selector must retain its type
! metadata, remain alive throughout the construct, and be finalized once when
! control leaves the construct.
!
! CHECK: 6 8 396
! IR_CHECK: call @afs_modproc_ar43_associate_derived_expr_m_finish_token
! IR_CHECK: call @afs_modproc_ar43_associate_derived_expr_m_finish_tokens
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
module ar43_associate_derived_expr_m
  implicit none
  integer :: construction_count = 0
  integer :: finalization_count = 0
  integer :: finalized_value_sum = 0

  type :: token
    integer :: value = 0
  contains
    final :: finish_token, finish_tokens
  end type token

  interface point_to
    module procedure point_to_token
  end interface point_to
contains
  function make_token(value) result(result_value)
    integer, intent(in) :: value
    type(token) :: result_value

    construction_count = construction_count + 1
    result_value%value = value
  end function make_token

  function point_to_token(target_value) result(result_value)
    type(token), target, intent(inout) :: target_value
    type(token), pointer :: result_value

    result_value => target_value
  end function point_to_token

  function make_tokens(first, second) result(values)
    integer, intent(in) :: first, second
    type(token), allocatable :: values(:)

    construction_count = construction_count + 2
    allocate(values(2))
    values(1)%value = first
    values(2)%value = second
  end function make_tokens

  subroutine finish_token(value)
    type(token), intent(inout) :: value

    finalization_count = finalization_count + 1
    finalized_value_sum = finalized_value_sum + value%value
  end subroutine finish_token

  subroutine finish_tokens(values)
    type(token), intent(inout) :: values(:)
    integer :: index

    finalization_count = finalization_count + size(values)
    do index = 1, size(values)
      finalized_value_sum = finalized_value_sum + values(index)%value
    end do
  end subroutine finish_tokens

  subroutine normal_case
    associate (value => make_token(11))
      if (construction_count /= 1) error stop 1
      if (finalization_count /= 0) error stop 2
      if (value%value /= 11) error stop 3
    end associate
    if (finalization_count /= 1) error stop 4
  end subroutine normal_case

  subroutine named_exit_case
    named: associate (value => make_token(22))
      if (construction_count /= 2) error stop 5
      if (finalization_count /= 1) error stop 6
      if (value%value /= 22) error stop 7
      exit named
      error stop 8
    end associate named
    if (finalization_count /= 2) error stop 9
  end subroutine named_exit_case

  subroutine goto_case
    associate (value => make_token(33))
      if (construction_count /= 3) error stop 10
      if (finalization_count /= 2) error stop 11
      if (value%value /= 33) error stop 12
      go to 100
      error stop 13
    end associate
    error stop 14
100 continue
    if (finalization_count /= 3) error stop 15
  end subroutine goto_case

  subroutine return_case
    associate (value => make_token(44), values => make_tokens(66, 77))
      if (construction_count /= 6) error stop 16
      if (finalization_count /= 3) error stop 17
      if (value%value /= 44) error stop 18
      if (size(values) /= 2) error stop 19
      if (values(1)%value /= 66) error stop 20
      if (values(2)%value /= 77) error stop 21
      return
    end associate
  end subroutine return_case

  subroutine pointer_result_case
    type(token), target :: target_value

    target_value%value = 55
    associate (value => point_to(target_value))
      if (finalization_count /= 7) error stop 27
      if (value%value /= 55) error stop 28
    end associate
    if (finalization_count /= 7) error stop 29
  end subroutine pointer_result_case

  subroutine parenthesized_value_case
    type(token) :: original

    original%value = 88
    associate (value => (original))
      if (value%value /= 88) error stop 22
      if (original%value /= 88) error stop 23
      if (finalization_count /= 6) error stop 24
    end associate
    if (original%value /= 88) error stop 25
    if (finalization_count /= 6) error stop 26
  end subroutine parenthesized_value_case
end module ar43_associate_derived_expr_m

program ar43_associate_derived_expr
  use ar43_associate_derived_expr_m
  implicit none

  call normal_case
  call named_exit_case
  call goto_case
  call return_case
  call parenthesized_value_case
  call pointer_result_case

  print *, construction_count, finalization_count, finalized_value_sum
  if (construction_count /= 6) error stop 30
  if (finalization_count /= 8) error stop 31
  if (finalized_value_sum /= 396) error stop 32
end program ar43_associate_derived_expr
