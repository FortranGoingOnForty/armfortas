! CHECK: narrow=T F T
! CHECK: scalar=T F T
module imported_logical_kind_metadata_m
  implicit none

  logical(1) :: narrow(3) = [.true._1, .false._1, .true._1]
  logical(8) :: wide = .true._8
  logical :: normal = .false.
  logical(1), parameter :: enabled = .true._1
end module imported_logical_kind_metadata_m

program imported_logical_kind_metadata
  use imported_logical_kind_metadata_m, only: imported_narrow => narrow, &
       imported_wide => wide, imported_normal => normal, imported_enabled => enabled
  implicit none

  print '(a,3(l1,1x))', 'narrow=', imported_narrow
  print '(a,3(l1,1x))', 'scalar=', imported_wide, imported_normal, imported_enabled
end program imported_logical_kind_metadata
