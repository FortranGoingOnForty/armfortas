! CHECK: copied=one two
! CHECK: source=ONE two
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
module fixed_derived_component_array_deep_copy_m
  implicit none

  type :: leaf_t
    character(:), allocatable :: text
  end type leaf_t

  type :: outer_t
    type(leaf_t) :: items(2)
  end type outer_t
end module fixed_derived_component_array_deep_copy_m

program fixed_derived_component_array_deep_copy
  use fixed_derived_component_array_deep_copy_m
  implicit none

  type(outer_t) :: source, copied

  source%items(1)%text = 'one'
  source%items(2)%text = 'two'
  copied = source
  source%items(1)%text = 'ONE'

  print '(a,a,1x,a)', 'copied=', copied%items(1)%text, copied%items(2)%text
  print '(a,a,1x,a)', 'source=', source%items(1)%text, source%items(2)%text
  if (copied%items(1)%text /= 'one') error stop 1
  if (copied%items(2)%text /= 'two') error stop 2
end program fixed_derived_component_array_deep_copy
