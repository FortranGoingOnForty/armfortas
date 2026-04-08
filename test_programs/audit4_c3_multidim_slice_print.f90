! Audit #4 CRITICAL-3 — multi-dimensional slice print silently
! emits empty output.
!
! The Maj-4 fix from audit #3 added a 1-D slice handler that
! requires `args.len() == 1`. Multi-dim slices fall through to
! the existing afs_create_section path, which the runtime can't
! consume on a bare stack array (it expects a 384-byte
! ArrayDescriptor and reads 384 bytes of garbage). The subsequent
! afs_write_string call gets len=0 and produces nothing.
!
! Either lower_1d_slice_write needs to generalize to N-D, or the
! print path needs to build a real ArrayDescriptor before calling
! afs_create_section.
!
! For a 3x3 column-major matrix:
!   m(1,1)=11 m(1,2)=12 m(1,3)=13
!   m(2,1)=21 m(2,2)=22 m(2,3)=23
!   m(3,1)=31 m(3,2)=32 m(3,3)=33
!
! XFAIL: audit CRITICAL-3 (multi-dim slice print emits nothing)
! CHECK: 11 21 31
! CHECK: 11 12 13
! CHECK: 21 22 31 32
program audit4_c3_multidim_slice_print
  integer :: m(3,3)
  integer :: i, j
  do j = 1, 3
    do i = 1, 3
      m(i, j) = i * 10 + j
    end do
  end do
  print *, m(:, 1)
  print *, m(1, :)
  print *, m(2:3, 1:2)
end program
