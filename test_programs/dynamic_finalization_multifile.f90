! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! MULTIFILE_LINK: dynamic_final_parent.f90 dynamic_final_child.f90 dynamic_final_main.f90
! CHECK: finals=1 1 3 3 1
! CHECK: scalar-copy=2 0 2 8
! CHECK: scalar-self-copy=2 0 2 2
! CHECK: scalar-alias-copy=2 0 2 2
! CHECK: rank-copy=0 2 4 17
! CHECK: rank-self-copy=0 2 4 16
! CHECK: rank-self-section=0 2 4 16

!--- file: dynamic_final_parent.f90
module dynamic_final_parent_m
  implicit none

  type :: parent_t
    integer :: parent_value = 0
  end type parent_t

contains

  subroutine release_scalar(value)
    class(parent_t), allocatable, intent(inout) :: value
    deallocate(value)
  end subroutine release_scalar

  subroutine release_rank_one(values)
    class(parent_t), allocatable, intent(inout) :: values(:)
    deallocate(values)
  end subroutine release_rank_one

  subroutine assign_scalar(destination, source)
    class(parent_t), allocatable, intent(out) :: destination
    class(parent_t), allocatable, intent(in) :: source
    destination = source
  end subroutine assign_scalar

  subroutine assign_scalar_pointer(destination, source)
    class(parent_t), allocatable, target, intent(inout) :: destination
    class(parent_t), pointer, intent(in) :: source
    destination = source
  end subroutine assign_scalar_pointer

  subroutine assign_rank_one(destination, source)
    class(parent_t), allocatable, intent(out) :: destination(:)
    class(parent_t), allocatable, intent(in) :: source(:)
    destination = source
  end subroutine assign_rank_one

  subroutine reverse_rank_one(values)
    class(parent_t), allocatable, intent(inout) :: values(:)
    values = values(2:1:-1)
  end subroutine reverse_rank_one

  subroutine self_assign_rank_one(values)
    class(parent_t), allocatable, intent(inout) :: values(:)
    values = values
  end subroutine self_assign_rank_one

end module dynamic_final_parent_m

!--- file: dynamic_final_child.f90
module dynamic_final_child_m
  use dynamic_final_parent_m, only: parent_t
  implicit none

  integer :: child_scalar_hits = 0
  integer :: child_rank_hits = 0
  integer :: payload_final_hits = 0
  integer :: payload_hits = 0

  type :: payload_t
    integer :: value = 0
  contains
    final :: finish_payload
  end type payload_t

  type, extends(parent_t) :: child_t
    type(payload_t), allocatable :: payload
  contains
    final :: finish_child_scalar, finish_child_rank_one
  end type child_t

contains

  subroutine finish_payload(value)
    type(payload_t), intent(inout) :: value
    payload_final_hits = payload_final_hits + 1
    payload_hits = payload_hits + value%value
  end subroutine finish_payload

  subroutine finish_child_scalar(value)
    type(child_t), intent(inout) :: value
    child_scalar_hits = child_scalar_hits + 1
  end subroutine finish_child_scalar

  subroutine finish_child_rank_one(values)
    type(child_t), intent(inout) :: values(:)
    child_rank_hits = child_rank_hits + 1
  end subroutine finish_child_rank_one

  subroutine make_scalar(value)
    class(parent_t), allocatable, intent(out) :: value
    allocate(child_t :: value)
    select type (value)
    type is (child_t)
      allocate(value%payload)
      value%payload%value = 1
    end select
  end subroutine make_scalar

  subroutine make_rank_one(values)
    class(parent_t), allocatable, intent(out) :: values(:)
    allocate(child_t :: values(2))
    select type (values)
    type is (child_t)
      allocate(values(1)%payload)
      allocate(values(2)%payload)
      values(1)%payload%value = 1
      values(2)%payload%value = 1
    end select
  end subroutine make_rank_one

end module dynamic_final_child_m

!--- file: dynamic_final_main.f90
program dynamic_final_main
  use dynamic_final_parent_m
  use dynamic_final_child_m
  implicit none

  class(parent_t), allocatable :: scalar
  class(parent_t), allocatable :: values(:)
  integer :: selected

  call make_scalar(scalar)
  call release_scalar(scalar)
  call make_rank_one(values)
  selected = 0
  select type (values)
  type is (child_t)
    selected = 1
  end select
  call release_rank_one(values)

  print '(a,i0,1x,i0,1x,i0,1x,i0,1x,i0)', 'finals=', &
    child_scalar_hits, child_rank_hits, payload_final_hits, payload_hits, selected

  if (child_scalar_hits /= 1) error stop 1
  if (child_rank_hits /= 1) error stop 2
  if (payload_final_hits /= 3) error stop 3
  if (payload_hits /= 3) error stop 4
  if (selected /= 1) error stop 5

  call check_scalar_assignment()
  call check_scalar_self_assignment()
  call check_scalar_alias_assignment()
  call check_rank_one_assignment()
  call check_rank_one_self_assignment()
  call check_rank_one_self_section()

contains

  subroutine check_scalar_assignment()
    class(parent_t), allocatable :: source, destination

    child_scalar_hits = 0
    child_rank_hits = 0
    payload_final_hits = 0
    payload_hits = 0

    call make_scalar(source)
    call assign_scalar(destination, source)

    select type (source)
    type is (child_t)
      source%payload%value = 7
    class default
      error stop 10
    end select

    select type (destination)
    type is (child_t)
      if (.not. allocated(destination%payload)) error stop 11
      if (destination%payload%value /= 1) error stop 12
    class default
      error stop 13
    end select

    call release_scalar(source)
    if (allocated(source)) error stop 14

    select type (destination)
    type is (child_t)
      if (.not. allocated(destination%payload)) error stop 15
      if (destination%payload%value /= 1) error stop 16
    class default
      error stop 17
    end select

    call release_scalar(destination)
    if (allocated(destination)) error stop 18

    print '(a,i0,1x,i0,1x,i0,1x,i0)', 'scalar-copy=', &
      child_scalar_hits, child_rank_hits, payload_final_hits, payload_hits

    if (child_scalar_hits /= 2) error stop 19
    if (child_rank_hits /= 0) error stop 20
    if (payload_final_hits /= 2) error stop 21
    if (payload_hits /= 8) error stop 22
  end subroutine check_scalar_assignment

  subroutine check_scalar_self_assignment()
    class(parent_t), allocatable :: value

    child_scalar_hits = 0
    child_rank_hits = 0
    payload_final_hits = 0
    payload_hits = 0

    call make_scalar(value)
    value = value

    select type (value)
    type is (child_t)
      if (.not. allocated(value%payload)) error stop 23
      if (value%payload%value /= 1) error stop 24
    class default
      error stop 25
    end select

    call release_scalar(value)
    if (allocated(value)) error stop 26

    print '(a,i0,1x,i0,1x,i0,1x,i0)', 'scalar-self-copy=', &
      child_scalar_hits, child_rank_hits, payload_final_hits, payload_hits

    if (child_scalar_hits /= 2) error stop 27
    if (child_rank_hits /= 0) error stop 28
    if (payload_final_hits /= 2) error stop 29
    if (payload_hits /= 2) error stop 30
  end subroutine check_scalar_self_assignment

  subroutine check_scalar_alias_assignment()
    class(parent_t), allocatable, target :: value
    class(parent_t), pointer :: alias

    child_scalar_hits = 0
    child_rank_hits = 0
    payload_final_hits = 0
    payload_hits = 0

    call make_scalar(value)
    alias => value
    call assign_scalar_pointer(value, alias)
    nullify(alias)

    select type (value)
    type is (child_t)
      if (.not. allocated(value%payload)) error stop 31
      if (value%payload%value /= 1) error stop 32
    class default
      error stop 33
    end select

    call release_scalar(value)
    if (allocated(value)) error stop 34

    print '(a,i0,1x,i0,1x,i0,1x,i0)', 'scalar-alias-copy=', &
      child_scalar_hits, child_rank_hits, payload_final_hits, payload_hits

    if (child_scalar_hits /= 2) error stop 35
    if (child_rank_hits /= 0) error stop 36
    if (payload_final_hits /= 2) error stop 37
    if (payload_hits /= 2) error stop 38
  end subroutine check_scalar_alias_assignment

  subroutine check_rank_one_assignment()
    class(parent_t), allocatable :: source(:), destination(:)

    child_scalar_hits = 0
    child_rank_hits = 0
    payload_final_hits = 0
    payload_hits = 0

    call make_rank_one(source)
    call assign_rank_one(destination, source)

    select type (source)
    type is (child_t)
      source(1)%payload%value = 7
      source(2)%payload%value = 8
    class default
      error stop 40
    end select

    select type (destination)
    type is (child_t)
      if (size(destination) /= 2) error stop 41
      if (.not. allocated(destination(1)%payload)) error stop 42
      if (.not. allocated(destination(2)%payload)) error stop 43
      if (destination(1)%payload%value /= 1) error stop 44
      if (destination(2)%payload%value /= 1) error stop 45
    class default
      error stop 46
    end select

    call release_rank_one(source)
    if (allocated(source)) error stop 47

    select type (destination)
    type is (child_t)
      if (.not. allocated(destination(1)%payload)) error stop 48
      if (.not. allocated(destination(2)%payload)) error stop 49
      if (destination(1)%payload%value /= 1) error stop 50
      if (destination(2)%payload%value /= 1) error stop 51
    class default
      error stop 52
    end select

    call release_rank_one(destination)
    if (allocated(destination)) error stop 53

    print '(a,i0,1x,i0,1x,i0,1x,i0)', 'rank-copy=', &
      child_scalar_hits, child_rank_hits, payload_final_hits, payload_hits

    if (child_scalar_hits /= 0) error stop 54
    if (child_rank_hits /= 2) error stop 55
    if (payload_final_hits /= 4) error stop 56
    if (payload_hits /= 17) error stop 57
  end subroutine check_rank_one_assignment

  subroutine check_rank_one_self_assignment()
    class(parent_t), allocatable :: values(:)

    child_scalar_hits = 0
    child_rank_hits = 0
    payload_final_hits = 0
    payload_hits = 0

    call make_rank_one(values)
    select type (values)
    type is (child_t)
      values(1)%payload%value = 3
      values(2)%payload%value = 5
    class default
      error stop 58
    end select

    call self_assign_rank_one(values)

    select type (values)
    type is (child_t)
      if (values(1)%payload%value /= 3) error stop 59
      if (values(2)%payload%value /= 5) error stop 60
    class default
      error stop 61
    end select

    call release_rank_one(values)
    if (allocated(values)) error stop 62

    print '(a,i0,1x,i0,1x,i0,1x,i0)', 'rank-self-copy=', &
      child_scalar_hits, child_rank_hits, payload_final_hits, payload_hits

    if (child_scalar_hits /= 0) error stop 63
    if (child_rank_hits /= 2) error stop 64
    if (payload_final_hits /= 4) error stop 65
    if (payload_hits /= 16) error stop 66
  end subroutine check_rank_one_self_assignment

  subroutine check_rank_one_self_section()
    class(parent_t), allocatable :: values(:)

    child_scalar_hits = 0
    child_rank_hits = 0
    payload_final_hits = 0
    payload_hits = 0

    call make_rank_one(values)
    select type (values)
    type is (child_t)
      values(1)%payload%value = 3
      values(2)%payload%value = 5
    class default
      error stop 70
    end select

    call reverse_rank_one(values)

    select type (values)
    type is (child_t)
      if (values(1)%payload%value /= 5) error stop 71
      if (values(2)%payload%value /= 3) error stop 72
    class default
      error stop 73
    end select

    call release_rank_one(values)
    if (allocated(values)) error stop 74

    print '(a,i0,1x,i0,1x,i0,1x,i0)', 'rank-self-section=', &
      child_scalar_hits, child_rank_hits, payload_final_hits, payload_hits

    if (child_scalar_hits /= 0) error stop 75
    if (child_rank_hits /= 2) error stop 76
    if (payload_final_hits /= 4) error stop 77
    if (payload_hits /= 16) error stop 78
  end subroutine check_rank_one_self_section
end program dynamic_final_main
