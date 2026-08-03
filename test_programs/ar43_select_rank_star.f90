! An assumed-rank dummy associated with an assumed-size actual must select
! RANK(*). Ordinary rank-one and rank-two actuals must continue to select their
! specific-rank and default guards respectively.
!
! CHECK: 109 109 503 201 303
! IR_CHECK: const_int 100
! IR_CHECK: const_int 9
! IR_CHECK: const_int 503
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program ar43_select_rank_star
  implicit none
  integer :: values(3)
  integer :: matrix(2, 2)
  integer :: star_rank_one_result
  integer :: star_rank_two_result
  integer :: no_star_result
  integer :: rank_one_result
  integer :: default_result

  values = [1, 2, 3]
  matrix = reshape([4, 5, 6, 7], [2, 2])

  call forward_assumed_size_rank_one(values, star_rank_one_result, no_star_result)
  call forward_assumed_size_rank_two(matrix, star_rank_two_result)
  call classify(values, rank_one_result)
  call classify(matrix, default_result)

  print *, star_rank_one_result, star_rank_two_result, no_star_result, &
    rank_one_result, default_result
  if (star_rank_one_result /= 109) error stop 1
  if (star_rank_two_result /= 109) error stop 2
  if (no_star_result /= 503) error stop 3
  if (rank_one_result /= 201) error stop 4
  if (default_result /= 303) error stop 5
contains
  subroutine forward_assumed_size_rank_one(actual, star_result, default_result)
    integer, intent(in) :: actual(*)
    integer, intent(out) :: star_result
    integer, intent(out) :: default_result

    call classify(actual, star_result)
    call classify_without_star(actual, default_result)
  end subroutine forward_assumed_size_rank_one

  subroutine forward_assumed_size_rank_two(actual, result)
    integer, intent(in) :: actual(2, *)
    integer, intent(out) :: result

    call classify(actual, result)
  end subroutine forward_assumed_size_rank_two

  subroutine classify(selector, result)
    integer, intent(in) :: selector(..)
    integer, intent(out) :: result

    select rank (selector)
    rank (1)
      result = 201
    rank (*)
      result = 100 + 9 * rank(selector)
    rank default
      result = 303
    end select
  end subroutine classify

  subroutine classify_without_star(selector, result)
    integer, intent(in) :: selector(..)
    integer, intent(out) :: result

    select rank (selector)
    rank (1)
      result = 401
    rank default
      result = 503
    end select
  end subroutine classify_without_star
end program ar43_select_rank_star
