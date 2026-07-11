! CHECK: finalized=3
! CHECK: state=F T 7
! CHECK: finalized=8
! CHECK: owner=F T 11
! CHECK: poly-finalized=27 17
! CHECK: poly=F T 13 23
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
module move_alloc_finalization_m
  implicit none

  integer :: finalized = 0
  integer :: owners_finalized = 0

  type :: item_t
    integer :: value = 0
  contains
    final :: finish_item
  end type item_t

  type :: owner_t
    integer :: marker = 0
    type(item_t), allocatable :: item
  contains
    final :: finish_owner
  end type owner_t
contains
  subroutine finish_item(item)
    type(item_t), intent(inout) :: item
    finalized = finalized + item%value
  end subroutine finish_item

  subroutine finish_owner(owner)
    type(owner_t), intent(inout) :: owner
    owners_finalized = owners_finalized + owner%marker
  end subroutine finish_owner
end module move_alloc_finalization_m

program move_alloc_finalizes_destination
  use move_alloc_finalization_m
  implicit none

  type(item_t), allocatable :: from, to
  type(owner_t), allocatable :: from_owner, to_owner
  class(*), allocatable :: from_poly, to_poly

  allocate(from, to)
  from%value = 7
  to%value = 3

  call move_alloc(from, to)

  print '(a,i0)', 'finalized=', finalized
  print '(a,l1,1x,l1,1x,i0)', 'state=', allocated(from), allocated(to), to%value
  if (finalized /= 3) error stop 1
  if (allocated(from)) error stop 2
  if (.not. allocated(to)) error stop 3
  if (to%value /= 7) error stop 4

  allocate(from_owner, to_owner)
  allocate(from_owner%item, to_owner%item)
  from_owner%item%value = 11
  to_owner%item%value = 5

  call move_alloc(from_owner, to_owner)

  print '(a,i0)', 'finalized=', finalized
  print '(a,l1,1x,l1,1x,i0)', 'owner=', allocated(from_owner), allocated(to_owner), &
    to_owner%item%value
  if (finalized /= 8) error stop 5
  if (allocated(from_owner)) error stop 6
  if (.not. allocated(to_owner)) error stop 7
  if (.not. allocated(to_owner%item)) error stop 8
  if (to_owner%item%value /= 11) error stop 9

  allocate(owner_t :: from_poly, to_poly)
  select type (from_poly)
  type is (owner_t)
    from_poly%marker = 13
    allocate(from_poly%item)
    from_poly%item%value = 23
  class default
    error stop 10
  end select
  select type (to_poly)
  type is (owner_t)
    to_poly%marker = 17
    allocate(to_poly%item)
    to_poly%item%value = 19
  class default
    error stop 11
  end select

  call move_alloc(from_poly, to_poly)

  print '(a,i0,1x,i0)', 'poly-finalized=', finalized, owners_finalized
  if (finalized /= 27) error stop 12
  if (owners_finalized /= 17) error stop 13
  select type (to_poly)
  type is (owner_t)
    print '(a,l1,1x,l1,1x,i0,1x,i0)', 'poly=', allocated(from_poly), &
      allocated(to_poly), to_poly%marker, to_poly%item%value
    if (to_poly%marker /= 13) error stop 14
    if (.not. allocated(to_poly%item)) error stop 15
    if (to_poly%item%value /= 23) error stop 16
  class default
    error stop 17
  end select
end program move_alloc_finalizes_destination
