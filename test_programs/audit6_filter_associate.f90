! Audit #6 probe — USE ONLY filter walker covers ASSOCIATE
! association exprs. Same family as audit6_filter_forall.
!
! ERROR_EXPECTED: hidden
! ERROR_SPAN: 14:19
module audit6_filter_associate_mod
  integer :: visible = 1
  integer :: hidden = 999
end module audit6_filter_associate_mod

program audit6_filter_associate
  use audit6_filter_associate_mod, only: visible
  integer :: x
  associate (y => hidden)
    x = y
  end associate
  print *, x
end program
