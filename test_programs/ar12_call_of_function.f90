module ar12_call_of_function_m
contains
  function f() result(v)
    integer :: v
    v = 2
  end function
end module

program ar12_call_of_function
  use ar12_call_of_function_m
  implicit none
  call f()
end program

! ERROR_EXPECTED: function 'f' cannot be invoked with CALL
! ERROR_SPAN: 12:8
