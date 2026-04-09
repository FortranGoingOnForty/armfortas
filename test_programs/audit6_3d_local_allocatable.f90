! Audit #6 probe — 3-D LOCAL allocatable (sister to the module
! version). The local-vs-module distinction matters because the
! address of the descriptor reaches compute_flat_elem_offset via
! a different code path (alloca vs global_addr), and we want
! both to honor the runtime descriptor reads correctly.
!
! Same expected output as the module version.
!
! CHECK: 111 211 121 221 131 231 112 212 122 222 132 232 113 213 123 223 133 233 114 214 124 224 134 234
program audit6_3d_local_allocatable
  integer, allocatable :: m(:,:,:)
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
