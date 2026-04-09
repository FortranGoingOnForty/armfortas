! Audit #6 probe — USE ONLY filter walker covers SELECT CASE
! selectors. Same family as audit6_filter_forall.
!
! ERROR_EXPECTED: hidden
module audit6_filter_select_mod
  integer :: visible = 1
  integer :: hidden = 999
end module audit6_filter_select_mod

program audit6_filter_select_case
  use audit6_filter_select_mod, only: visible
  select case (hidden)
    case default
      print *, "default"
  end select
end program
