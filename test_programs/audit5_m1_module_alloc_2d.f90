! Audit #5 MAJOR-1 — 2-D module allocatable arrays produce
! garbage because allocate() uses afs_allocate_1d with the
! flattened total instead of building a real rank-2 descriptor.
!
! The audit-4 Maj-5 fix made 1-D module allocatables work but
! the local-allocatable multi-D path has a "fall back to 1-D"
! stub that allocate() inherits — it multiplies extents into a
! single n and calls afs_allocate_1d. The runtime descriptor
! is then a flat 12-element rank-1 desc with no stride info,
! and m(i,j) subscripting computes against uninitialized
! extents.
!
! Expected (column-major iteration of `print *, m`):
!   m(1,1)=11 m(2,1)=21 m(3,1)=31
!   m(1,2)=12 m(2,2)=22 m(3,2)=32
!   m(1,3)=13 m(2,3)=23 m(3,3)=33
!   m(1,4)=14 m(2,4)=24 m(3,4)=34
!
! XFAIL: audit5 MAJOR-1 (2-D module allocatable allocate falls back to 1-D)
! CHECK: 11 21 31 12 22 32 13 23 33 14 24 34
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
