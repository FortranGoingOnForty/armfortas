! x10a edge case: variable-count shifts. A shift by a register amount
! requires the count in cl (rcx); isel moves the count vreg into rcx
! and the shift reads %cl. With rcx in the allocation pool now, the
! fixed-interval check must keep other live values out of rcx across
! each shift, and the count vreg is hinted toward rcx (coalescing the
! move). Several shifts with surrounding live values stress this.
! Opaque seed prevents folding; OPT_EQ ties naive to linear scan.
! FLAGS: --std=f2023
! CHECK: r 1196
! CHECK: ok
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program x10a_shift_pressure
  implicit none
  integer :: r
  call sh(command_argument_count(), r)
  print '(A,1X,I0)', 'r', r
  print '(A)', 'ok'
contains
  subroutine sh(seed, out)
    integer, intent(in) :: seed
    integer, intent(out) :: out
    integer :: a, b, c, d, e, s1, s2, s3
    a = seed + 1       ! variable shift counts (live, go through rcx)
    b = seed + 2
    c = seed + 3
    d = seed + 16      ! shift subjects (live across the shifts)
    e = seed + 255
    s1 = ishft(d, a)   ! 16 << 1   = 32
    s2 = ishft(e, b)   ! 255 << 2  = 1020
    s3 = ishft(d, c)   ! 16 << 3   = 128
    out = s1 + s2 + s3 + iand(e, d)   ! 32 + 1020 + 128 + (255 .and. 16 = 16) = 1196
  end subroutine
end program x10a_shift_pressure
