! audit31 Finding 7: an IMPORT statement inside an interface body
! was reported as a parse error at the `::`. The real cause was
! that parse_function required RESULT before BIND in the function
! header; the test has `bind(...) result(r)`, which left `result(r)`
! unconsumed and shifted every subsequent line one state off. Once
! either ordering is accepted, the IMPORT parse path (already
! implemented in parse_unit_body phase 1.5) kicks in normally.
! Task #488.
! CHECK: ok
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
