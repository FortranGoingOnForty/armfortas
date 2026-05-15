! CHECK: ok
! REPRO_CHECK: run

program scalar_allocatable_payload_load
  implicit none

  real :: x(5)
  real, allocatable :: center
  real :: acc
  integer :: i

  x(1) = 1.0
  x(2) = 2.0
  x(3) = 3.0
  x(4) = 4.0
  x(5) = 5.0

  allocate(center, source = sum(x) / real(size(x)))

  acc = 0.0
  do i = 1, 5
    acc = acc + (x(i) - center) * (x(i) - center)
  end do

  if (.not. allocated(center)) error stop 1
  if (abs(center - 3.0) > 1.0e-5) error stop 2
  if (abs(acc / 5.0 - 2.0) > 1.0e-5) error stop 3

  print *, 'ok'
end program
