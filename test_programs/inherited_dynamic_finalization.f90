! CHECK: static=21 1 1
! CHECK: dynamic=21121 2 2
! CHECK: component=21 1 1
! CHECK: array=723
! CHECK: scalar-parent=5
! CHECK: scalar-parent-array=712
! CHECK: nonintegral-parent=30710 24
! CHECK: component-order=1234
! CHECK: fixed-array-components=2 7
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
module inherited_dynamic_finalization_m
  implicit none

  integer :: events = 0
  integer :: child_hits = 0
  integer :: parent_hits = 0
  integer :: array_events = 0
  integer :: scalar_parent_events = 0
  integer :: nonintegral_parent_events = 0
  integer :: nonintegral_child_events = 0
  integer :: component_order = 0
  integer :: fixed_payload_count = 0
  integer :: fixed_payload_sum = 0

  type :: parent_t
    integer :: parent_value = 0
  contains
    final :: finish_parent, finish_parent_array
  end type parent_t

  type, extends(parent_t) :: child_t
    integer :: child_value = 0
  contains
    final :: finish_child, finish_child_array
  end type child_t

  type :: holder_t
    class(parent_t), allocatable :: value
  end type holder_t

  type :: scalar_parent_t
    integer :: parent_value = 0
  contains
    final :: finish_scalar_parent
  end type scalar_parent_t

  type, extends(scalar_parent_t) :: rank_child_t
    integer :: child_value = 0
  contains
    final :: finish_rank_child_array
  end type rank_child_t

  type :: complex_parent_t
    complex :: value = (0.0, 0.0)
  contains
    final :: finish_complex_parent_array
  end type complex_parent_t

  type, extends(complex_parent_t) :: complex_child_t
    integer :: tail = 0
  contains
    final :: finish_complex_child_array
  end type complex_child_t

  type :: parent_component_t
  contains
    final :: finish_parent_component
  end type parent_component_t

  type :: child_component_t
  contains
    final :: finish_child_component
  end type child_component_t

  type :: parent_owner_t
    type(parent_component_t) :: component
  contains
    final :: finish_parent_owner
  end type parent_owner_t

  type, extends(parent_owner_t) :: child_owner_t
    type(child_component_t) :: child_component
  contains
    final :: finish_child_owner
  end type child_owner_t

  type :: fixed_payload_t
    integer :: value = 0
  contains
    final :: finish_fixed_payload
  end type fixed_payload_t

  type :: fixed_owner_t
    type(fixed_payload_t), allocatable :: payload
  end type fixed_owner_t
contains
  subroutine finish_parent(value)
    type(parent_t), intent(inout) :: value
    events = events * 10 + 1
    parent_hits = parent_hits + 1
  end subroutine finish_parent

  subroutine finish_child(value)
    type(child_t), intent(inout) :: value
    events = events * 10 + 2
    child_hits = child_hits + 1
  end subroutine finish_child

  subroutine finish_parent_array(values)
    type(parent_t), intent(inout) :: values(:)
    array_events = array_events * 100 + sum(values%parent_value) + 10 * values(2)%parent_value
  end subroutine finish_parent_array

  subroutine finish_child_array(values)
    type(child_t), intent(inout) :: values(:)
    array_events = array_events * 100 + sum(values%child_value)
  end subroutine finish_child_array

  subroutine finish_scalar_parent(value)
    type(scalar_parent_t), intent(inout) :: value
    scalar_parent_events = scalar_parent_events * 10 + value%parent_value
  end subroutine finish_scalar_parent

  subroutine finish_rank_child_array(values)
    type(rank_child_t), intent(inout) :: values(:)
    scalar_parent_events = scalar_parent_events * 10 + sum(values%child_value)
  end subroutine finish_rank_child_array

  subroutine finish_complex_parent_array(values)
    type(complex_parent_t), intent(inout) :: values(:)
    nonintegral_parent_events = int(real(values(1)%value)) * 100 + &
      int(real(values(2)%value))
    nonintegral_parent_events = nonintegral_parent_events * 100 + &
      int(sum(real(values%value)))
  end subroutine finish_complex_parent_array

  subroutine finish_complex_child_array(values)
    type(complex_child_t), intent(inout) :: values(:)
    nonintegral_child_events = sum(values%tail)
  end subroutine finish_complex_child_array

  subroutine finish_parent_component(value)
    type(parent_component_t), intent(inout) :: value
    component_order = component_order * 10 + 4
  end subroutine finish_parent_component

  subroutine finish_child_component(value)
    type(child_component_t), intent(inout) :: value
    component_order = component_order * 10 + 2
  end subroutine finish_child_component

  subroutine finish_parent_owner(value)
    type(parent_owner_t), intent(inout) :: value
    component_order = component_order * 10 + 3
  end subroutine finish_parent_owner

  subroutine finish_child_owner(value)
    type(child_owner_t), intent(inout) :: value
    component_order = component_order * 10 + 1
  end subroutine finish_child_owner

  subroutine finish_fixed_payload(value)
    type(fixed_payload_t), intent(inout) :: value
    fixed_payload_count = fixed_payload_count + 1
    fixed_payload_sum = fixed_payload_sum + value%value
  end subroutine finish_fixed_payload

  subroutine finalize_static_child()
    type(child_t) :: value
  end subroutine finalize_static_child

  subroutine finalize_dynamic_child()
    class(parent_t), allocatable :: value
    allocate(child_t :: value)
    deallocate(value)
  end subroutine finalize_dynamic_child

  subroutine finalize_dynamic_component()
    type(holder_t) :: holder
    allocate(child_t :: holder%value)
  end subroutine finalize_dynamic_component

  subroutine finalize_child_array()
    type(child_t), allocatable :: values(:)
    allocate(values(2))
    values(1)%parent_value = 1
    values(2)%parent_value = 2
    values(1)%child_value = 3
    values(2)%child_value = 4
    deallocate(values)
  end subroutine finalize_child_array

  subroutine finalize_scalar_parent()
    type(rank_child_t) :: value
    value%parent_value = 5
  end subroutine finalize_scalar_parent

  subroutine finalize_scalar_parent_array()
    type(rank_child_t), allocatable :: values(:)
    allocate(values(2))
    values(1)%parent_value = 1
    values(2)%parent_value = 2
    values(1)%child_value = 3
    values(2)%child_value = 4
    deallocate(values)
  end subroutine finalize_scalar_parent_array

  subroutine finalize_nonintegral_parent_array()
    class(complex_parent_t), allocatable :: values(:)
    allocate(complex_child_t :: values(2))
    select type (values)
    type is (complex_child_t)
      values(1)%value = (3.0, 0.0)
      values(2)%value = (7.0, 0.0)
      values(1)%tail = 11
      values(2)%tail = 13
    end select
    deallocate(values)
  end subroutine finalize_nonintegral_parent_array

  subroutine finalize_component_order()
    type(child_owner_t) :: value
  end subroutine finalize_component_order

  subroutine finalize_fixed_array_components()
    type(fixed_owner_t) :: values(2)
    allocate(values(1)%payload)
    allocate(values(2)%payload)
    values(1)%payload%value = 3
    values(2)%payload%value = 4
  end subroutine finalize_fixed_array_components
end module inherited_dynamic_finalization_m

program inherited_dynamic_finalization
  use inherited_dynamic_finalization_m
  implicit none

  integer :: static_events, static_child_hits, static_parent_hits

  call finalize_static_child()
  print '(a,i0,1x,i0,1x,i0)', 'static=', events, child_hits, parent_hits
  static_events = events
  static_child_hits = child_hits
  static_parent_hits = parent_hits

  events = events * 10 + 1
  call finalize_dynamic_child()
  print '(a,i0,1x,i0,1x,i0)', 'dynamic=', events, child_hits, parent_hits
  if (static_events /= 21) error stop 1
  if (static_child_hits /= 1 .or. static_parent_hits /= 1) error stop 2
  if (events /= 21121) error stop 3
  if (child_hits /= 2 .or. parent_hits /= 2) error stop 4

  events = 0
  child_hits = 0
  parent_hits = 0
  call finalize_dynamic_component()
  print '(a,i0,1x,i0,1x,i0)', 'component=', events, child_hits, parent_hits
  if (events /= 21 .or. child_hits /= 1 .or. parent_hits /= 1) error stop 5

  call finalize_child_array()
  print '(a,i0)', 'array=', array_events
  if (array_events /= 723) error stop 6

  call finalize_scalar_parent()
  print '(a,i0)', 'scalar-parent=', scalar_parent_events
  if (scalar_parent_events /= 5) error stop 7

  scalar_parent_events = 0
  call finalize_scalar_parent_array()
  print '(a,i0)', 'scalar-parent-array=', scalar_parent_events
  if (scalar_parent_events /= 712) error stop 8

  call finalize_nonintegral_parent_array()
  print '(a,i0,1x,i0)', 'nonintegral-parent=', &
    nonintegral_parent_events, nonintegral_child_events
  if (nonintegral_parent_events /= 30710) error stop 9
  if (nonintegral_child_events /= 24) error stop 10

  call finalize_component_order()
  print '(a,i0)', 'component-order=', component_order
  if (component_order /= 1234) error stop 11

  call finalize_fixed_array_components()
  print '(a,i0,1x,i0)', 'fixed-array-components=', &
    fixed_payload_count, fixed_payload_sum
  if (fixed_payload_count /= 2 .or. fixed_payload_sum /= 7) error stop 12
end program inherited_dynamic_finalization
