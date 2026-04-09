! Test large stack frame (>32KB) prologue/epilogue (BLOCKING fix)
! Arrays totaling >32KB force stp_offset > 32760, which previously
! emitted a "; FIXME" stub instead of valid instructions.
program large_frame
  implicit none
  call fill_and_sum()
contains

  subroutine fill_and_sum()
    real :: a(2000)   ! 8000 bytes
    real :: b(2000)   ! 8000 bytes
    real :: c(2000)   ! 8000 bytes
    real :: d(1024)   ! 4096 bytes
    real :: e(1200)   ! 4800 bytes  => ~32.8KB total: triggers large-frame path
    real, parameter :: tol = 1.0e-6
    integer :: i
    real :: s

    do i = 1, 2000
      a(i) = real(i)
      b(i) = real(i) * 2.0
      c(i) = real(i) * 3.0
    end do
    do i = 1, 1024
      d(i) = real(i) * 0.5
    end do
    do i = 1, 1200
      e(i) = 1.0
    end do

    ! a(1)=1, b(1)=2, c(1)=3, d(1)=0.5, e(1)=1.0 => 7.5
    s = a(1) + b(1) + c(1) + d(1) + e(1)
    if (abs(s - 7.5) < tol) then
      print *, "PASS"
    else
      print *, "FAIL", s
    end if

    ! a(2000)+b(2000)+c(2000) = 2000+4000+6000 = 12000
    s = a(2000) + b(2000) + c(2000)
    if (abs(s - 12000.0) < tol) then
      print *, "PASS"
    else
      print *, "FAIL", s
    end if

    ! Boundary: last element of e
    if (abs(e(1200) - 1.0) < tol) then
      print *, "PASS"
    else
      print *, "FAIL", e(1200)
    end if
  end subroutine fill_and_sum

end program large_frame
! CHECK: PASS
! CHECK: PASS
! CHECK: PASS
