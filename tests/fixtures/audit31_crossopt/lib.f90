! audit31 cross-opt compatibility: library side
! Used with audit31_brutal_crossopt_main.f90.
module audit31_crossopt_lib
  implicit none
  type :: box_t
    integer :: tag = 0
    real(8) :: payload = 0.0d0
    character(len=16) :: label = ''
  end type
contains
  ! Scalar in, scalar out
  function double_add(a, b) result(r)
    real(8), intent(in) :: a, b
    real(8) :: r
    r = a + b
  end function

  ! Explicit-shape array in (avoid assumed-shape which has a separate bug)
  subroutine sum_arr(xs, n, r)
    integer, intent(in) :: n
    integer, intent(in) :: xs(n)
    integer, intent(out) :: r
    integer :: i
    r = 0
    do i = 1, n
      r = r + xs(i)
    end do
  end subroutine

  ! Character arg, returns integer
  function clen(s) result(r)
    character(len=*), intent(in) :: s
    integer :: r
    r = len_trim(s)
  end function

  ! Derived type in, derived type out
  function copy_box(b) result(out)
    type(box_t), intent(in) :: b
    type(box_t) :: out
    out%tag = b%tag + 1
    out%payload = b%payload * 2.0d0
    out%label = b%label
  end function

  ! Subroutine with intent(out) array — explicit shape
  subroutine fill_arr(xs, n, v)
    integer, intent(in) :: n, v
    integer, intent(out) :: xs(n)
    integer :: i
    do i = 1, n
      xs(i) = v + i
    end do
  end subroutine
end module
