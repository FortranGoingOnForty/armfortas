! CHECK: ok
! IR_CHECK: derived_alloc_array_copy
! IR_CHECK: call @afs_allocate_like
! IR_CHECK: call @afs_assign_char_deferred
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
module derived_alloc_array_component_deep_copy_m
  implicit none

  type :: string_t
    character(:), allocatable :: raw
  end type

  type :: list_t
    type(string_t), allocatable :: items(:)
  end type

contains
  subroutine fill(list, first, second)
    type(list_t), intent(inout) :: list
    character(*), intent(in) :: first
    character(*), intent(in) :: second

    if (.not. allocated(list%items)) allocate(list%items(2))
    list%items(1)%raw = first
    list%items(2)%raw = second
  end subroutine
end module

program derived_alloc_array_component_deep_copy
  use derived_alloc_array_component_deep_copy_m
  implicit none

  type(list_t) :: src
  type(list_t) :: dst

  call fill(src, "aa", "bb")
  dst = src

  call fill(src, "xx", "yy")

  if (dst%items(1)%raw /= "aa") error stop 1
  if (dst%items(2)%raw /= "bb") error stop 2
  if (src%items(1)%raw /= "xx") error stop 3
  if (src%items(2)%raw /= "yy") error stop 4

  print *, "ok"
end program
