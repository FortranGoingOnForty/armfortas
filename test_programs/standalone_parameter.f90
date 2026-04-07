! Standalone PARAMETER (x = ...) statement at statement-start.
! parse_parameter_stmt existed but was never called from the main
! statement dispatcher in parse_unit_body, so the statement was
! silently dropped and the program ran with x = 0.
! Audit MAJOR-2.
!
! CHECK: 42
! CHECK: 100
program standalone_parameter
  implicit none
  integer :: x
  integer :: y
  parameter (x = 42)
  parameter (y = 100)

  print *, x
  print *, y
end program
