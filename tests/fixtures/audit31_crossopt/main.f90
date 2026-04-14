! audit31 cross-opt compatibility: main side
program audit31_crossopt_main
  use audit31_crossopt_lib
  implicit none
  integer :: xs(5), total
  type(box_t) :: b, c
  real(8) :: r

  r = double_add(1.5d0, 2.25d0)
  print *, 'double_add=', r

  xs = (/1, 2, 3, 4, 5/)
  call sum_arr(xs, 5, total)
  print *, 'sum_arr=', total

  print *, 'clen=', clen('hello world   ')

  b%tag = 10
  b%payload = 3.0d0
  b%label = 'boxlabel'
  c = copy_box(b)
  print *, 'copy_box=', c%tag, c%payload, c%label

  call fill_arr(xs, 5, 100)
  print *, 'fill_arr=', xs
end program
