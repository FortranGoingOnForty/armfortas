! Audit #5 MAJOR-1 — 2-D module allocatable arrays.
!
! Fixed: the multi-D allocate path in lower.rs now builds a
! stack DimDescriptor[rank] with (lower=1, upper=N_i, stride=1)
! per dim and calls afs_allocate_array directly, instead of
! flattening extents into afs_allocate_1d. The descriptor now
! holds the real rank and per-dim extents so m(i,j) subscripting
! resolves correctly.
!
! Expected (column-major iteration of `print *, m`):
!   m(1,1)=11 m(2,1)=21 m(3,1)=31
!   m(1,2)=12 m(2,2)=22 m(3,2)=32
!   m(1,3)=13 m(2,3)=23 m(3,3)=33
!   m(1,4)=14 m(2,4)=24 m(3,4)=34
!
! IR-shape assertions (audit5 MIN-2):
!   * Multi-D allocate must call afs_allocate_array with a
!     stack DimDescriptor[rank], NOT fall back to afs_allocate_1d.
!     (The pre-fix flatten-and-call-1d path was the bug.)
!
! CHECK: 11 21 31 12 22 32 13 23 33 14 24 34
! IR_CHECK: call @afs_allocate_array
! IR_NOT: call @afs_allocate_1d
module audit5_m1_mod
  integer, allocatable :: m(:,:)
end module audit5_m1_mod

program audit5_m1_module_alloc_2d
  use audit5_m1_mod
  integer :: i, j
  allocate(m(3, 4))
  do j = 1, 4
    do i = 1, 3
      m(i, j) = i*10 + j
    end do
  end do
  print *, m
end program
