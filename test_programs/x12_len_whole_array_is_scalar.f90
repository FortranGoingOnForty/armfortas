! Regression: LEN of a whole character array is a scalar (the element
! length), not an element-wise array. LEN was grouped with the truly
! elemental character intrinsics (LEN_TRIM, INDEX, ...), so LEN(w) for
! a rank-1 array w expanded element-wise and wrote `5 5 5` instead of
! the scalar 5. LEN is an inquiry function (F2018 §16.9.108). x12.
!
! CHECK: direct=5
! CHECK: viavar=5
! CHECK: total=15
program t
  implicit none
  character(len=:), allocatable :: w(:)
  integer :: l
  allocate(character(len=5) :: w(3))
  l = len(w)
  write(*, '(a,i0)') 'direct=', len(w)
  write(*, '(a,i0)') 'viavar=', l
  ! len(w) is scalar, so multiplying by size reproduces total storage
  write(*, '(a,i0)') 'total=', len(w) * size(w)
end program
