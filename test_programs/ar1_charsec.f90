! CHECK: fixed=def:jkl
! CHECK: star=mno:abc
! CHECK: words=one def thr jkl fiv
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
module ar1_charsec_m
contains
  subroutine fixed_explicit(a, n)
    integer, intent(in) :: n
    character(len=3), intent(in) :: a(n)

    print '(4a)', 'fixed=', a(1), ':', a(n)
  end subroutine fixed_explicit

  subroutine star_assumed_size(a, n)
    integer, intent(in) :: n
    character(len=*), intent(in) :: a(*)

    print '(4a)', 'star=', a(1), ':', a(n)
  end subroutine star_assumed_size

  subroutine rewrite_explicit(a, n)
    integer, intent(in) :: n
    character(len=3), intent(inout) :: a(n)

    if (n >= 1) a(1) = 'one'
    if (n >= 2) a(2) = 'thr'
    if (n >= 3) a(3) = 'fiv'
  end subroutine rewrite_explicit
end module ar1_charsec_m

program ar1_charsec
  use ar1_charsec_m
  implicit none

  character(len=3) :: words(5)

  words = [character(len=3) :: 'abc', 'def', 'ghi', 'jkl', 'mno']
  call fixed_explicit(words(2:4), 3)
  call star_assumed_size(words(5:1:-2), 3)
  call rewrite_explicit(words(1:5:2), 3)
  print '(a,5(a,1x))', 'words=', words
end program ar1_charsec
