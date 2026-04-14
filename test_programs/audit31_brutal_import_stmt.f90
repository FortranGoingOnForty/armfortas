! XFAIL: F2003 IMPORT statement not parsed — Task #488
! CHECK: 42
! audit31: F2003 IMPORT statement inside interface block not parsed.
! Blocks fortsh c-interop (signals, coprocess, signal_handling, directory_builtin,
! command_builtin, etc.) — every program using named C interfaces
! with BIND(C) functions taking iso_c_binding kinds.
! Expected: compile cleanly.
! Actual:   "parse error: expected expression, got ::" at the IMPORT line.
module audit31_import
  use iso_c_binding
  implicit none
  interface
    function foo_c(x) bind(C, name="foo") result(r)
      import :: c_int
      integer(c_int), value :: x
      integer(c_int) :: r
    end function
  end interface
end module

program p
  use audit31_import
  implicit none
  print *, 'ok'
end program
