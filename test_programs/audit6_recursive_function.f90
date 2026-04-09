! Audit #6 probe — recursive function.
!
! Verifies the calling convention round-trip on a function that
! calls itself with a smaller argument and returns the product.
! Tests stack frame setup, return-value materialization, and
! intra-procedural recursion through `contains`.
!
! fact(5) = 5*4*3*2*1 = 120.
!
! CHECK: 120
program audit6_recursive_function
  print *, fact(5)
contains
  recursive function fact(n) result(f)
    integer :: n, f
    if (n <= 1) then
      f = 1
    else
      f = n * fact(n-1)
    end if
  end function fact
end program
