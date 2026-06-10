! Sprint x05 curated program: ~48KB of locals — over ARM's historical
! 32KB cliff, exercising the x86 stack-probe loop. (Heap threshold is
! 64KB, so these stay stack-resident.)
! CHECK: 12276
! CHECK: 18
program x05_big_frame
  implicit none
  integer :: a(6144), b(6144)
  integer :: i
  do i = 1, 6144
    a(i) = i
    b(i) = 6144 - i + 1
  end do
  print *, a(17) + b(17) + a(6144) + b(1) - 6157
  print *, a(9) + b(6136)
end program x05_big_frame
