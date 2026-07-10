! CHECK: finalized=7
! CHECK: lhs=7 8
! CHECK: self_finalized=22
! CHECK: result_finalized=37
! CHECK: result=9 10
! CHECK: owned_finalized=3
! CHECK: owned=five six
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
module allocatable_array_assignment_finalization_m
  implicit none

  integer :: finalized = 0
  integer :: owned_finalized = 0

  type :: item_t
    integer :: value = 0
  contains
    final :: finish_items
  end type item_t

  type :: owner_t
    integer :: value = 0
    character(:), allocatable :: text
  contains
    final :: finish_owners
  end type owner_t
contains
  subroutine finish_items(items)
    type(item_t), intent(inout) :: items(:)
    finalized = finalized + sum(items%value)
  end subroutine finish_items

  subroutine finish_owners(owners)
    type(owner_t), intent(inout) :: owners(:)
    owned_finalized = owned_finalized + sum(owners%value)
  end subroutine finish_owners

  function make_items() result(items)
    type(item_t), allocatable :: items(:)
    allocate(items(2))
    items(1)%value = 9
    items(2)%value = 10
  end function make_items
end module allocatable_array_assignment_finalization_m

program allocatable_array_assignment_finalization
  use allocatable_array_assignment_finalization_m
  implicit none

  type(item_t), allocatable :: lhs(:), rhs(:)
  type(owner_t), allocatable :: lhs_owner(:), rhs_owner(:)

  allocate(lhs(2), rhs(2))
  lhs(1)%value = 3
  lhs(2)%value = 4
  rhs(1)%value = 7
  rhs(2)%value = 8

  lhs = rhs

  print '(a,i0)', 'finalized=', finalized
  print '(a,i0,1x,i0)', 'lhs=', lhs%value
  if (finalized /= 7) error stop 1
  if (any(lhs%value /= [7, 8])) error stop 2

  lhs = lhs
  print '(a,i0)', 'self_finalized=', finalized
  if (finalized /= 22) error stop 3
  if (any(lhs%value /= [7, 8])) error stop 4

  lhs = make_items()
  print '(a,i0)', 'result_finalized=', finalized
  print '(a,i0,1x,i0)', 'result=', lhs%value
  if (finalized /= 37) error stop 5
  if (any(lhs%value /= [9, 10])) error stop 6

  allocate(lhs_owner(2), rhs_owner(2))
  lhs_owner(1)%value = 1
  lhs_owner(2)%value = 2
  lhs_owner(1)%text = 'one'
  lhs_owner(2)%text = 'two'
  rhs_owner(1)%value = 5
  rhs_owner(2)%value = 6
  rhs_owner(1)%text = 'five'
  rhs_owner(2)%text = 'six'

  lhs_owner = rhs_owner
  rhs_owner(1)%text = 'changed'

  print '(a,i0)', 'owned_finalized=', owned_finalized
  print '(a,a,1x,a)', 'owned=', lhs_owner(1)%text, lhs_owner(2)%text
  if (owned_finalized /= 3) error stop 7
  if (lhs_owner(1)%text /= 'five') error stop 8
  if (lhs_owner(2)%text /= 'six') error stop 9
end program allocatable_array_assignment_finalization
