! audit31 Finding 4: `inner` inside `outer` inside `program` used
! to lose `host_var` writes because the closure-passing walker
! only collected refs against the IMMEDIATE host. Pass the full
! ancestor decl chain into walk_contained_host_refs, and fold
! each proc's nested-contained refs into its own so intermediate
! levels carry the outer-scope vars as hidden params for
! forwarding. Task #485.
! CHECK: 20
program audit31_nested_host
  implicit none
  integer :: host_var = 10
  call outer()
  print *, 'final host_var=', host_var
contains
  subroutine outer()
    call inner()
  contains
    subroutine inner()
      host_var = host_var * 2
    end subroutine
  end subroutine
end program
