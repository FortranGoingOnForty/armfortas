! CHECK: formatted_end
! CHECK: list_end
! CHECK: stream_end
! CHECK: iostat_end
! CHECK: err_only_iostat_end
! REPRO_CHECK: run_same_sandbox
program ar4_read_end
  use, intrinsic :: iso_fortran_env, only : iostat_end
  implicit none

  if (command_argument_count() > 0) then
    call nohandler_formatted()
    error stop 99
  end if

  call formatted_end_case()
  call list_end_case()
  call stream_end_case()
  call iostat_case()
  call err_only_iostat_case()

contains

  subroutine formatted_end_case()
    integer :: u, x

    open(newunit=u, status='scratch', action='readwrite', form='formatted')
    write(u, '(i4)') 42
    rewind(u)
    read(u, '(i4)') x
    if (x /= 42) error stop 1
    read(u, '(i4)', end=100) x
    error stop 2
100 print '(a)', 'formatted_end'
    close(u)
  end subroutine

  subroutine list_end_case()
    integer :: u, x

    open(newunit=u, status='scratch', action='readwrite', form='formatted')
    write(u, *) 7
    rewind(u)
    read(u, *) x
    if (x /= 7) error stop 3
    read(u, *, end=110) x
    error stop 4
110 print '(a)', 'list_end'
    close(u)
  end subroutine

  subroutine stream_end_case()
    integer :: u
    character(len=3) :: text

    open(newunit=u, status='scratch', access='stream', form='unformatted', &
         action='readwrite')
    write(u) 'abc'
    rewind(u)
    read(u) text
    if (text /= 'abc') error stop 5
    read(u, end=120) text
    error stop 6
120 print '(a)', 'stream_end'
    close(u)
  end subroutine

  subroutine iostat_case()
    integer :: u, ios, x

    open(newunit=u, status='scratch', action='readwrite', form='formatted')
    rewind(u)
    ios = 0
    x = 0
    read(u, *, iostat=ios) x
    if (ios /= iostat_end) error stop 7
    print '(a)', 'iostat_end'
    close(u)
  end subroutine

  subroutine err_only_iostat_case()
    integer :: u, ios, x

    open(newunit=u, status='scratch', action='readwrite', form='formatted')
    rewind(u)
    ios = 0
    x = 0
    read(u, *, iostat=ios, err=200) x
    if (ios /= iostat_end) error stop 8
    print '(a)', 'err_only_iostat_end'
    close(u)
    return
200 error stop 9
  end subroutine

  subroutine nohandler_formatted()
    integer :: u, x

    open(newunit=u, status='scratch', action='readwrite', form='formatted')
    rewind(u)
    x = 123
    read(u, '(i4)') x
    print '(a)', 'nohandler_missed'
  end subroutine

end program
