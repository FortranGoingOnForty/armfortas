! CHECK: derf= 1 1 2 3 4
! CHECK: derr= 2 3 4 5 5
! CHECK: charf= a1 a1 b2 c3 d4
! CHECK: charr= b2 c3 d4 e5 e5
! CHECK: ok
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program ar8_overlap_section_derived_char
  implicit none

  type :: cell
    integer :: n
    character(len=2) :: tag
  end type cell

  type(cell) :: p(5)
  character(len=2) :: c(5)
  integer :: i

  call init_cells(p)
  p(2:5) = p(1:4)
  if (p(1)%n /= 1 .or. p(2)%n /= 1 .or. p(3)%n /= 2) error stop 1
  if (p(4)%n /= 3 .or. p(5)%n /= 4) error stop 2
  print '(a,5(1x,i0))', 'derf=', p(1)%n, p(2)%n, p(3)%n, p(4)%n, p(5)%n

  call init_cells(p)
  p(1:4) = p(2:5)
  if (p(1)%n /= 2 .or. p(2)%n /= 3 .or. p(3)%n /= 4) error stop 3
  if (p(4)%n /= 5 .or. p(5)%n /= 5) error stop 4
  print '(a,5(1x,i0))', 'derr=', p(1)%n, p(2)%n, p(3)%n, p(4)%n, p(5)%n

  c = ['a1', 'b2', 'c3', 'd4', 'e5']
  c(2:5) = c(1:4)
  if (c(1) /= 'a1' .or. c(2) /= 'a1' .or. c(3) /= 'b2') error stop 5
  if (c(4) /= 'c3' .or. c(5) /= 'd4') error stop 6
  print '(a,5(1x,a))', 'charf=', c(1), c(2), c(3), c(4), c(5)

  c = ['a1', 'b2', 'c3', 'd4', 'e5']
  c(1:4) = c(2:5)
  if (c(1) /= 'b2' .or. c(2) /= 'c3' .or. c(3) /= 'd4') error stop 7
  if (c(4) /= 'e5' .or. c(5) /= 'e5') error stop 8
  print '(a,5(1x,a))', 'charr=', c(1), c(2), c(3), c(4), c(5)

  print '(a)', 'ok'

contains
  subroutine init_cells(a)
    type(cell), intent(out) :: a(:)

    do i = 1, size(a)
      a(i)%n = i
      write(a(i)%tag, '(a,i1)') 'v', i
    end do
  end subroutine init_cells
end program ar8_overlap_section_derived_char
