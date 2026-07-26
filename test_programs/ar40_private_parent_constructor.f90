! AR40-06: an extended-type constructor initializes the whole parent
! component, not the first inherited physical field.
!
! CHECK: 1 3
! CHECK: 1 4
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
module parent_ctor_owner
  implicit none
  private
  public :: make_parent, read_parent

  type, public :: parent_t
    integer, private :: hidden = 1
  end type parent_t
contains
  function make_parent() result(item)
    type(parent_t) :: item
  end function make_parent

  integer function read_parent(item)
    type(parent_t), intent(in) :: item
    read_parent = item%hidden
  end function read_parent
end module parent_ctor_owner

module child_ctor_owner
  use parent_ctor_owner, only: parent_t, make_parent
  implicit none
  private
  public :: make_child_positional, make_child_keyword

  type, extends(parent_t), public :: child_t
    integer, public :: shown = 2
  end type child_t
contains
  function make_child_positional() result(item)
    type(child_t) :: item
    item = child_t(make_parent(), 3)
  end function make_child_positional

  function make_child_keyword() result(item)
    type(child_t) :: item
    item = child_t(parent_t=make_parent(), shown=4)
  end function make_child_keyword
end module child_ctor_owner

program exercise_parent_constructor
  use parent_ctor_owner, only: read_parent
  use child_ctor_owner, only: child_t, make_child_positional, make_child_keyword
  implicit none
  type(child_t) :: positional, keyword

  positional = make_child_positional()
  keyword = make_child_keyword()
  print *, read_parent(positional%parent_t), positional%shown
  print *, read_parent(keyword%parent_t), keyword%shown
end program exercise_parent_constructor
