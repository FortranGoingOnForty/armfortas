! L-tail arity gate exemption: a visible user procedure shadows the
! intrinsic name, so its own signature governs. One-argument atan2
! here is legal and must compile and run.
program ltail_intrinsic_arity_shadow_ok
  implicit none
  print '(i0)', atan2(20)
! CHECK: 21
  print '(i0)', maxi(3)
! CHECK: 4
contains
  integer function atan2(n)
    integer, intent(in) :: n
    atan2 = n + 1
  end function atan2
  integer function maxi(n)
    integer, intent(in) :: n
    maxi = n + 1
  end function maxi
end program ltail_intrinsic_arity_shadow_ok
