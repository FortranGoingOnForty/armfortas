! Regression: PRINT with an explicit format must honor it even when an
! output item is a function call. The lowering used to force the
! list-directed path whenever an item contained a procedure call
! (protecting a single-global format engine from nested I/O during item
! evaluation), so `print '(a,f8.3)', 'area = ', area(r)` emitted
! E-notation with list-directed spacing in every real program that
! prints a computed value. The runtime's FMT_CTX is a stack now — nested
! internal writes run in their own context — so the fallback is gone.
module m_geom
  implicit none
contains
  real function area(r)
     real, intent(in) :: r
     area = 3.14159265 * r * r
  end function
  function tag(n) result(t)
     integer, intent(in) :: n
     character(len=:), allocatable :: t
     character(len=32) :: buf
     ! nested INTERNAL formatted write during output-item evaluation:
     ! must not disturb the enclosing PRINT's format state.
     write(buf, '(a,i3.3,a)') '<', n, '>'
     t = trim(buf)
  end function
end module

program p
  use m_geom
  implicit none
  print '(a,f8.3)', 'circle area = ', area(2.0)
  print '(a,1x,a,1x,f6.2)', 'item', tag(42), 2.5
  print '(a)', 'DONE'
end program
! CHECK: circle area =   12.566
! CHECK: item <042>   2.50
! CHECK: DONE
