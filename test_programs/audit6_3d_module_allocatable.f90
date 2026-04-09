! Audit #6 probe — 3-D MODULE allocatable. Verifies that the
! audit5 MAJOR-1 compute_flat_elem_offset rewrite (which moved
! allocatable subscript math to a runtime descriptor read) holds
! up beyond rank 2. Column-major iteration over m(2, 3, 4):
!
!   k outermost: 1, 2, 3, 4
!   j middle:    1, 2, 3
!   i innermost: 1, 2
!
! Storage order with m(i,j,k) = i*100 + j*10 + k:
!   k=1: 111 211 121 221 131 231
!   k=2: 112 212 122 222 132 232
!   k=3: 113 213 123 223 133 233
!   k=4: 114 214 124 224 134 234
!
! CHECK: 111 211 121 221 131 231 112 212 122 222 132 232 113 213 123 223 133 233 114 214 124 224 134 234
module audit6_3d_mod_mod
  integer, allocatable :: m(:,:,:)
end module audit6_3d_mod_mod

program audit6_3d_module_allocatable
  use audit6_3d_mod_mod
  integer :: i, j, k
  allocate(m(2,3,4))
  do k = 1, 4
    do j = 1, 3
      do i = 1, 2
        m(i,j,k) = i*100 + j*10 + k
      end do
    end do
  end do
  print *, m
end program
