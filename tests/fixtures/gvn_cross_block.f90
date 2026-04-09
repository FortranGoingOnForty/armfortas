integer function gvn_cross_block(a, b, flag)
  implicit none
  integer, value :: a, b, flag
  integer :: tmp

  tmp = a + b
  if (flag > 0) then
    gvn_cross_block = tmp + (a + b)
  else
    gvn_cross_block = tmp - (a + b)
  end if
end function gvn_cross_block
