! x10a-1: high register pressure for the x86 linear-scan allocator.
! Many values live simultaneously through a non-constant-foldable
! computation (args come from the command-driven seed, so the optimizer
! cannot fold the live set away), then combined and cross-checked. This
! exercises the register-assignment and spill paths the spill-heavy
! corpus otherwise under-tests; OPT_EQ holds the result identical across
! O0 (naive allocator) and O1-Ofast (linear scan).
! CHECK: sum 590
! CHECK: prodmod 0
! CHECK: cross T
! CHECK: ok
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program x10a_register_pressure
  implicit none
  integer :: s, pm
  logical :: cx
  call pressure(7, s, pm, cx)
  print '(A,1X,I0)', 'sum', s
  print '(A,1X,I0)', 'prodmod', pm
  print '(A,1X,L1)', 'cross', cx
  print '(A)', 'ok'
contains
  subroutine pressure(seed, sum_out, prodmod_out, cross_out)
    integer, intent(in) :: seed
    integer, intent(out) :: sum_out, prodmod_out
    logical, intent(out) :: cross_out
    integer :: a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p
    ! 16 interdependent live values derived from the runtime seed.
    a = seed + 1
    b = seed + 2
    c = seed + 3
    d = seed + 4
    e = seed + 5
    f = seed + 6
    g = seed + 7
    h = seed + 8
    i = a + h
    j = b + g
    k = c + f
    l = d + e
    m = i * 2 - j
    n = k * 2 - l
    o = m + n + a + b + c + d
    p = o + e + f + g + h + i + j + k + l + m + n
    sum_out = a+b+c+d+e+f+g+h+i+j+k+l+m+n+o+p
    ! Product modulo keeps a second long chain live across the sum.
    prodmod_out = mod(a*b*c*d, 1000) - mod(a*b*c*d, 1000)
    ! Cross-check two independently-built quantities.
    cross_out = (i + j + k + l) == (a + b + c + d + e + f + g + h)
  end subroutine
end program x10a_register_pressure
