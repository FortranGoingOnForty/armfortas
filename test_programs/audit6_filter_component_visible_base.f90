! Audit #6 probe — the filter walker should NOT fire on a
! component access whose base is visible, even if the
! *component name* happens to collide with a filtered module
! variable name. ComponentAccess only walks the base, never
! the component name string.
!
! `x` (the base) is visible via USE ONLY. `hidden_field` is a
! component name on type t — NOT a module-level name. The
! walker must not flag this as a filtered reference.
!
! CHECK: 0
module audit6_filter_comp_mod
  type :: audit6_t
    integer :: hidden_field = 0
  end type audit6_t
  type(audit6_t) :: x
  integer :: hidden = 999
end module audit6_filter_comp_mod

program audit6_filter_component_visible_base
  use audit6_filter_comp_mod, only: x
  print *, x%hidden_field
end program
