! Audit C3: whole component array section in list-directed and formatted
! output. `print *, a%id` where `a` is an array and `id` a scalar component
! is a strided view (consecutive `id`s are one record apart, not one int).
! The output list lowering had no case for a ComponentAccess on an array
! base, so it fell through to the scalar dispatch and wrote only the first
! element: `print *, a%id` printed `10` instead of `10 20 30`. The
! descriptor was already correct (`sum(a%id)` worked), so the fix walks the
! section by its record stride in both the list-directed and push-based
! formatted output paths. Covers rank-1 and rank-2 bases and components at
! a nonzero offset (real/logical here); numeric element kinds.
program component_array_section_io
  type point
    integer :: id
    real :: w
    logical :: on
  end type
  type(point) :: a(3)
  type(point) :: g(2, 2)
  integer :: i, j, k

  do i = 1, 3
    a(i)%id = i*10
    a(i)%w = real(i) + 0.5
    a(i)%on = (mod(i, 2) == 0)
  end do

  ! list-directed integer component section
  print *, 'LD', a%id
  ! CHECK: LD 10 20 30

  ! formatted integer / real / logical component sections (nonzero offsets)
  print '(A,3I4)', 'FI', a%id
  ! CHECK: FI  10  20  30
  print '(A,3F6.1)', 'FW', a%w
  ! CHECK: FW   1.5   2.5   3.5
  print '(A,3L2)', 'FL', a%on
  ! CHECK: FL F T F

  ! rank-2 base: element order is column-major (g(1,1) g(2,1) g(1,2) g(2,2))
  k = 0
  do j = 1, 2
    do i = 1, 2
      k = k + 1
      g(i, j)%id = k*100
    end do
  end do
  print '(A,4I5)', 'G2', g%id
  ! CHECK: G2  100  200  300  400

  ! WRITE with an explicit format takes the same push path
  write (*, '(A,3I4)') 'WR', a%id
  ! CHECK: WR  10  20  30
end program
