! x10a edge case: integer and real(8) values live simultaneously, so
! the allocator juggles the GP pool (rax/rcx/.../r15) and the XMM pool
! (xmm0-13) at once, plus their separate spill scratch. Mixed pressure
! exercises both class paths together. Opaque seed prevents folding;
! OPT_EQ ties naive to linear scan.
! FLAGS: --std=f2023
! CHECK: isum 72
! CHECK: fok T
! CHECK: ok
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program x10a_mixed_int_float_pressure
  implicit none
  integer :: isum
  real(8) :: fsum
  call mix(command_argument_count(), isum, fsum)
  print '(A,1X,I0)', 'isum', isum
  ! fsum = 2*(1+..+8) = 72.0 (real cross-check via tolerance).
  print '(A,1X,L1)', 'fok', abs(fsum - 72.0d0) < 1.0d-9
  print '(A)', 'ok'
contains
  subroutine mix(seed, isum_out, fsum_out)
    integer, intent(in) :: seed
    integer, intent(out) :: isum_out
    real(8), intent(out) :: fsum_out
    integer :: i1, i2, i3, i4, i5, i6, i7, i8
    real(8) :: x1, x2, x3, x4, x5, x6, x7, x8
    i1 = seed + 1; i2 = seed + 2; i3 = seed + 3; i4 = seed + 4
    i5 = seed + 5; i6 = seed + 6; i7 = seed + 7; i8 = seed + 8
    x1 = real(i8, 8); x2 = real(i7, 8); x3 = real(i6, 8); x4 = real(i5, 8)
    x5 = real(i4, 8); x6 = real(i3, 8); x7 = real(i2, 8); x8 = real(i1, 8)
    ! Interleave so both classes stay live across each other.
    isum_out = i1 + i2 + i3 + i4 + i5 + i6 + i7 + i8 &
             + nint(x1) + nint(x2) + nint(x3) + nint(x4) &
             + nint(x5) + nint(x6) + nint(x7) + nint(x8)
    fsum_out = (x1 + x2 + x3 + x4 + x5 + x6 + x7 + x8) &
             + real(i1 + i2 + i3 + i4 + i5 + i6 + i7 + i8, 8)
  end subroutine
end program x10a_mixed_int_float_pressure
