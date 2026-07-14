! CHECK: result=10 1243567890
module block_goto_cleanup_mod
  implicit none

  integer :: final_count = 0
  integer :: final_order = 0

  type :: marker
    integer :: tag = 0
  contains
    final :: finish_marker
  end type marker
contains
  subroutine finish_marker(value)
    type(marker), intent(inout) :: value

    final_count = final_count + 1
    final_order = final_order * 10 + value%tag
  end subroutine finish_marker

  subroutine direct_outward_jump()
    block
      type(marker) :: value

      value%tag = 1
      go to 100
    end block
    error stop 1
100 continue
  end subroutine direct_outward_jump

  subroutine same_block_jump()
    block
      type(marker) :: value

      value%tag = 2
      go to 200
      error stop 2
200   continue
      if (final_count /= 1) error stop 3
    end block
  end subroutine same_block_jump

  subroutine nested_outward_jump()
    block
      type(marker) :: outer

      outer%tag = 3
      block
        type(marker) :: inner

        inner%tag = 4
        go to 300
      end block
      error stop 4
    end block
    error stop 5
300 continue
  end subroutine nested_outward_jump

  subroutine computed_jump(selector)
    integer, intent(in) :: selector

    block
      type(marker) :: value

      value%tag = 4 + selector
      go to (400, 500), selector
      error stop 6
400   continue
      if (final_count /= 4) error stop 7
    end block
    return
500 continue
  end subroutine computed_jump

  subroutine shadowed_outward_jump()
    type(marker) :: value

    value%tag = 8
    block
      type(marker) :: value

      value%tag = 7
      go to 600
    end block
    error stop 13
600 continue
  end subroutine shadowed_outward_jump

  subroutine shadowed_normal_exit()
    type(marker) :: value

    value%tag = 0
    block
      type(marker) :: value

      value%tag = 9
    end block
    if (final_count /= 9 .or. final_order /= 124356789) error stop 14
  end subroutine shadowed_normal_exit
end module block_goto_cleanup_mod

program block_goto_cleanup
  use block_goto_cleanup_mod
  implicit none

  call direct_outward_jump()
  if (final_count /= 1 .or. final_order /= 1) error stop 8

  call same_block_jump()
  if (final_count /= 2 .or. final_order /= 12) error stop 9

  call nested_outward_jump()
  if (final_count /= 4 .or. final_order /= 1243) error stop 10

  call computed_jump(1)
  if (final_count /= 5 .or. final_order /= 12435) error stop 11

  call computed_jump(2)
  if (final_count /= 6 .or. final_order /= 124356) error stop 12

  call shadowed_outward_jump()
  if (final_count /= 8 .or. final_order /= 12435678) error stop 18

  call shadowed_normal_exit()
  if (final_count /= 10 .or. final_order /= 1243567890) error stop 19

  print '(a,i0,1x,i0)', 'result=', final_count, final_order
end program block_goto_cleanup
