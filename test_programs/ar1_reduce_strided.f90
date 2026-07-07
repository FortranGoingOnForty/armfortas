! CHECK: real_pos=99 225000 50 1
! CHECK: real_neg=99 225000 50 1
! CHECK: int_pos=99 225000 50 1
! CHECK: int_neg=99 225000 50 1
! CHECK: logic_pos=4 1 0
! CHECK: logic_neg=4 1 0
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
module ar1_reduce_strided_m
contains
  subroutine show_real(label, a)
    character(len=*), intent(in) :: label
    real(8), intent(in) :: a(:, :)

    print '(a,4(i0,1x))', label, int(sum(a)), int(product(a)), int(maxval(a)), int(minval(a))
  end subroutine show_real

  subroutine show_int(label, a)
    character(len=*), intent(in) :: label
    integer, intent(in) :: a(:, :)

    print '(a,4(i0,1x))', label, sum(a), product(a), maxval(a), minval(a)
  end subroutine show_int

  subroutine show_logical(label, a)
    character(len=*), intent(in) :: label
    logical, intent(in) :: a(:, :)
    integer :: any_flag
    integer :: all_flag

    any_flag = 0
    all_flag = 0
    if (any(a)) any_flag = 1
    if (all(a)) all_flag = 1
    print '(a,3(i0,1x))', label, count(a), any_flag, all_flag
  end subroutine show_logical
end module ar1_reduce_strided_m

program ar1_reduce_strided
  use ar1_reduce_strided_m
  implicit none

  real(8) :: rm(2, 5)
  integer :: im(2, 5)
  logical :: lm(2, 5)

  rm(1, :) = [1.0d0, 2.0d0, 3.0d0, 4.0d0, 5.0d0]
  rm(2, :) = [10.0d0, 20.0d0, 30.0d0, 40.0d0, 50.0d0]
  im(1, :) = [1, 2, 3, 4, 5]
  im(2, :) = [10, 20, 30, 40, 50]

  lm = .false.
  lm(1, 1) = .true.
  lm(2, 1) = .true.
  lm(1, 3) = .true.
  lm(2, 5) = .true.

  call show_real('real_pos=', rm(:, 1:5:2))
  call show_real('real_neg=', rm(2:1:-1, 5:1:-2))
  call show_int('int_pos=', im(:, 1:5:2))
  call show_int('int_neg=', im(2:1:-1, 5:1:-2))
  call show_logical('logic_pos=', lm(:, 1:5:2))
  call show_logical('logic_neg=', lm(2:1:-1, 5:1:-2))
end program ar1_reduce_strided
