! `procedure(iface), pointer :: f, g` declares two procedure
! pointers in one statement. The parser used to stop after the first
! name and then trip on the comma with "expected expression, got ,".
! CHECK: 1
program t
  implicit none
  abstract interface
    integer function unary_op(x)
      integer, intent(in) :: x
    end function
  end interface
  procedure(unary_op), pointer :: f, g
  print *, 1
end program
