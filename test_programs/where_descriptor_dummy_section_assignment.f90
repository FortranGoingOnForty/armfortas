! CHECK: ok
! IR_CHECK: where_body
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program where_descriptor_dummy_section_assignment
  implicit none
  real :: lambda(2), s(2)

  lambda = [8.0, 0.0]
  call fill_s(lambda, s)

  if (abs(s(1) - 4.0) > 1.0e-6) error stop 1
  if (abs(s(2)) > 1.0e-6) error stop 2

  write(*, "(a)") "ok"
contains
  subroutine fill_s(lambda, singular_values)
    real, intent(in) :: lambda(:)
    real, intent(out) :: singular_values(:)
    integer :: m, n

    n = 3
    m = size(singular_values, 1)
    singular_values = 0.0
    where (lambda(1:m) > 0.0)
      singular_values(1:m) = sqrt(lambda(1:m) * real(n - 1))
    end where
  end subroutine fill_s
end program where_descriptor_dummy_section_assignment
